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
    NebulaWorkspace, NewWindow, OpenSettings, TAB_LABEL_ICON_SIZE, TAB_LABEL_ICON_W, TabDrag,
    TabDragAxis, TabPresentation, ToggleShellPicker, pane_header,
};

pub(super) const TOP_TAB_H: f32 = 34.0;
/// 与顶部模式正文卡片的 `px_2` 左边距同源，让首个 tab 和终端左缘对齐。
pub(super) const TOP_TAB_LEFT_INSET: f32 = 8.0;
/// 单个 tab 的最小宽。WT 的 TabView 同一条取舍：标签**不压到读不出来**，
/// 宁可溢出让用户翻页。120 曾经是下限，但分屏 tab 的行首 2×2 图标 + 尾部
/// 数量胶囊各要 20 多像素，120 里标题只剩两三个字符（实测挤成 `te`）。
/// 翻页按钮到位之后，早一点溢出比早一点读不出来划算。
const TOP_TAB_MIN_W: f32 = 160.0;
const TOP_TAB_MAX_W: f32 = 220.0;
const TOP_TAB_GAP: f32 = 4.0;
const TOP_TAB_STATUS_W: f32 = 28.0;
/// 标题栏中不属于 tab 视口的固定预算：左内边距、四枚 32px 操作按钮、
/// 三枚 34px 窗口按钮，以及至少 72px 的可拖拽空白。
const TOP_TAB_RESERVED_W: f32 = TOP_TAB_LEFT_INSET + 32.0 * 4.0 + 34.0 * 3.0 + 72.0;
/// 溢出翻页按钮（WT 的 TabView 在 tab 溢出时于两端给 `‹ ›`）单枚占的宽。
const TOP_TAB_NUDGE_W: f32 = 22.0;
/// 拖拽越界自动滚的触发带宽：指针进到视口左/右这么近就开始滚。
const TOP_TAB_EDGE_BAND: f32 = 24.0;

fn tab_width(viewport_w: f32, count: usize) -> f32 {
    if count == 0 || viewport_w <= 1.0 {
        return TOP_TAB_MAX_W;
    }
    (viewport_w / count as f32).clamp(TOP_TAB_MIN_W, TOP_TAB_MAX_W)
}

fn tab_strip_width(tab_w: f32, count: usize) -> f32 {
    tab_w * count as f32 + TOP_TAB_GAP * count.saturating_sub(1) as f32
}

/// tab 条是否装不下。装不下才画翻页按钮，也才允许自动滚。
fn strip_overflows(strip_w: f32, capacity_w: f32) -> bool {
    strip_w > capacity_w + 0.5
}

/// GPUI 横向滚动偏移是 ≤ 0（内容向左移），与 [`NebulaWorkspace::on_top_tabs_wheel`]
/// 同一套符号。可滚范围因此是 `[-(strip - viewport), 0]`。
fn clamp_strip_offset(x: f32, strip_w: f32, viewport_w: f32) -> f32 {
    let max = (strip_w - viewport_w).max(0.0);
    x.clamp(-max, 0.0)
}

/// 一次翻页：`dir` 为 -1 向左（露出前面的 tab）、+1 向右。
fn nudge_offset(x: f32, step: f32, dir: f32, strip_w: f32, viewport_w: f32) -> f32 {
    clamp_strip_offset(x - dir * step, strip_w, viewport_w)
}

/// 已经贴住某一端时按钮要灰掉——一枚点了没反应的箭头比没有箭头更让人怀疑
/// 是不是卡了。
fn at_strip_start(x: f32) -> bool {
    x >= -0.5
}

fn at_strip_end(x: f32, strip_w: f32, viewport_w: f32) -> bool {
    x <= -(strip_w - viewport_w).max(0.0) + 0.5
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
        let chrome_family = theme.mono_font_family.clone();
        let symbol_family: SharedString = crate::font_install::REQUIRED_FONT_FAMILY.into();
        let label_px = settings.map(|settings| settings.base_font_size_px).unwrap_or(15.0);
        let tab_capacity_w =
            (f32::from(window.viewport_size().width) - TOP_TAB_RESERVED_W).max(TOP_TAB_MIN_W);
        let tab_w = tab_width(tab_capacity_w, self.tabs.len());
        let strip_w = tab_strip_width(tab_w, self.tabs.len());
        // 溢出时两端各让出一枚翻页按钮。这个反馈是单调的：`tab_w` 已被
        // `TOP_TAB_MIN_W` 钳死，扣掉按钮宽只会让溢出更成立，不会在"画了按钮
        // → 不溢出了 → 撤掉按钮"之间抖。
        let overflow = strip_overflows(strip_w, tab_capacity_w);
        let tab_viewport_w = if overflow {
            (tab_capacity_w - TOP_TAB_NUDGE_W * 2.0).max(TOP_TAB_MIN_W)
        } else {
            strip_w.min(tab_capacity_w)
        };
        let scroll_x = f32::from(self.top_tabs_scroll.offset().x);
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
                    pane_count,
                } = self.tab_presentation(ix, cx, dark);
                let hover_group: SharedString = format!("top-tab-hover-{ix}").into();
                let cross_window_drag = self.cross_window_drag_payload(ix, cx);
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
                            .font_family(chrome_family.clone())
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
                    .font_family(chrome_family.clone())
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
                    // 图标优先级与侧栏同源：先身份（跟随聚焦 pane），分屏标记
                    // 只在没有身份图标时补位。理由见 sidebar.rs 同处注释。
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
                    .when_some(logo_image.clone(), |row, image| {
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
                                .font_family(symbol_family.clone())
                                .text_size(px(label_px))
                                .font_weight(FontWeight::NORMAL)
                                .text_color(if active { active_fg } else { muted })
                                .child(glyph),
                        )
                    })
                    .when(
                        pane_count > 1
                            && !is_settings
                            && logo_image.is_none()
                            && program_glyph.is_none(),
                        |row| {
                            row.child(
                                div()
                                    .w(px(TAB_LABEL_ICON_W))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(pane_header::split_glyph(
                                        label_px * 0.78,
                                        if active { active_fg } else { muted },
                                    )),
                            )
                        },
                    )
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
                                    .font_family(chrome_family.clone()),
                            )
                            .into_any_element(),
                        None => div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(chrome_family.clone())
                            .text_size(px(label_px))
                            .font_weight(FontWeight::LIGHT)
                            .child(title)
                            .into_any_element(),
                    })
                    .when(pane_count > 1, |row| {
                        row.child(
                            div()
                                .id(("top-pane-count", ix))
                                .flex_shrink_0()
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.cycle_pane_focus(ix, window, cx);
                                }))
                                .child(pane_header::split_badge(
                                    pane_count,
                                    label_px,
                                    if active { active_fg } else { muted },
                                    if active { active_bg } else { theme.muted },
                                )),
                        )
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
                    )
                    .when_some(cross_window_drag, |row, payload| {
                        row.on_drag(payload, |payload, _, _, cx| {
                            NebulaWorkspace::cross_window_drag_preview(payload, cx)
                        })
                    });

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
                    .flex_shrink(1.0)
                    .min_w_0()
                    .items_end()
                    // 溢出翻页：只在装不下时出现。滚轮已经能滚，但滚轮是个
                    // 看不见的入口——tab 一多，用户根本不知道右边还有东西。
                    .when(overflow, |bar| {
                        bar.child(
                            div().h(px(TOP_TAB_H)).flex().items_center().child(
                                Button::new("top-tabs-prev")
                                    .icon(IconName::ChevronLeft)
                                    .ghost()
                                    .xsmall()
                                    .disabled(at_strip_start(scroll_x))
                                    .tooltip("向左翻标签")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.nudge_top_tabs(
                                            -1.0,
                                            pitch,
                                            strip_w,
                                            tab_viewport_w,
                                            cx,
                                        );
                                    })),
                            ),
                        )
                    })
                    .child(
                        div()
                            .id("top-tabs-viewport")
                            .relative()
                            .w(px(tab_viewport_w))
                            .flex_shrink(1.0)
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
                    .when(overflow, |bar| {
                        bar.child(
                            div().h(px(TOP_TAB_H)).flex().items_center().child(
                                Button::new("top-tabs-next")
                                    .icon(IconName::ChevronRight)
                                    .ghost()
                                    .xsmall()
                                    .disabled(at_strip_end(scroll_x, strip_w, tab_viewport_w))
                                    .tooltip("向右翻标签")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.nudge_top_tabs(
                                            1.0,
                                            pitch,
                                            strip_w,
                                            tab_viewport_w,
                                            cx,
                                        );
                                    })),
                            ),
                        )
                    })
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
                                gpui::Anchor::TopRight,
                                move |menu, _, _| {
                                    let shell_picker = menu_workspace.clone();
                                    let new_window = menu_workspace.clone();
                                    let settings = menu_workspace.clone();
                                    menu.external_link_icon(false)
                                        .item(
                                            PopupMenuItem::new("新建窗口")
                                                .icon(IconName::Plus)
                                                .action(Box::new(NewWindow))
                                                .on_click(move |_, _, cx| {
                                                    if new_window.upgrade().is_some() {
                                                        cx.defer(|cx| {
                                                            if let Err(error) = crate::gpui_shell::workspace::windowing::open_new_window(cx, None) {
                                                                log::warn!("failed to open GPUI window: {error}");
                                                            }
                                                        });
                                                    }
                                                }),
                                        )
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
                    .icon(IconName::Github)
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

    /// 翻页按钮的落点：一次一个 tab 步距，钳在可滚范围内。
    fn nudge_top_tabs(
        &mut self,
        dir: f32,
        step: f32,
        strip_w: f32,
        viewport_w: f32,
        cx: &mut Context<Self>,
    ) {
        let mut offset = self.top_tabs_scroll.offset();
        let next = nudge_offset(f32::from(offset.x), step, dir, strip_w, viewport_w);
        if (next - f32::from(offset.x)).abs() < 0.5 {
            return;
        }
        offset.x = px(next);
        self.top_tabs_scroll.set_offset(offset);
        cx.notify();
    }

    /// 横向拖拽越界自动滚：指针进到视口左/右边缘 [`TOP_TAB_EDGE_BAND`] 内就
    /// 按半个步距推一次。翻页按钮解决的是"看不见后面还有 tab"，这条解决的是
    /// "拖着 tab 推不过滚动边界"——两者少一个，tab 一多都还是废的。
    ///
    /// 视口几何这里现算：`update_tab_drag` 拿不到 render 时的那份局部量，而
    /// 两处的减法必须同源，否则边缘带会和真实视口错开。
    pub(super) fn autoscroll_top_tabs_for_drag(
        &mut self,
        pointer_x: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.tabs_position != nebula_settings::TabsPositionName::Top {
            return;
        }
        let capacity_w =
            (f32::from(window.viewport_size().width) - TOP_TAB_RESERVED_W).max(TOP_TAB_MIN_W);
        let tab_w = tab_width(capacity_w, self.tabs.len());
        let strip_w = tab_strip_width(tab_w, self.tabs.len());
        if !strip_overflows(strip_w, capacity_w) {
            return;
        }
        let viewport_w = (capacity_w - TOP_TAB_NUDGE_W * 2.0).max(TOP_TAB_MIN_W);
        // 视口左缘 = 左内边距 + 左侧翻页按钮。
        let left = TOP_TAB_LEFT_INSET + TOP_TAB_NUDGE_W;
        let right = left + viewport_w;
        let dir = if pointer_x <= left + TOP_TAB_EDGE_BAND {
            -1.0
        } else if pointer_x >= right - TOP_TAB_EDGE_BAND {
            1.0
        } else {
            return;
        };
        self.nudge_top_tabs(dir, (tab_w + TOP_TAB_GAP) * 0.5, strip_w, viewport_w, cx);
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

    #[test]
    fn nudge_buttons_only_exist_once_the_strip_overflows() {
        assert!(!strip_overflows(600.0, 800.0));
        assert!(!strip_overflows(800.0, 800.0));
        assert!(strip_overflows(801.0, 800.0));
    }

    #[test]
    fn strip_offset_stays_inside_the_scrollable_range() {
        // 可滚 200px：偏移合法区间是 [-200, 0]。
        assert_eq!(clamp_strip_offset(50.0, 1000.0, 800.0), 0.0);
        assert_eq!(clamp_strip_offset(-500.0, 1000.0, 800.0), -200.0);
        assert_eq!(clamp_strip_offset(-120.0, 1000.0, 800.0), -120.0);
        // 装得下时根本不该有偏移。
        assert_eq!(clamp_strip_offset(-30.0, 600.0, 800.0), 0.0);
    }

    #[test]
    fn nudge_walks_one_step_and_stops_at_both_ends() {
        assert_eq!(nudge_offset(0.0, 124.0, 1.0, 1000.0, 800.0), -124.0);
        assert_eq!(nudge_offset(-124.0, 124.0, -1.0, 1000.0, 800.0), 0.0);
        // 末端：再往右也只到 -200。
        assert_eq!(nudge_offset(-150.0, 124.0, 1.0, 1000.0, 800.0), -200.0);
        // 首端：再往左也只到 0。
        assert_eq!(nudge_offset(-50.0, 124.0, -1.0, 1000.0, 800.0), 0.0);
    }

    #[test]
    fn end_state_drives_the_disabled_arrows() {
        assert!(at_strip_start(0.0));
        assert!(!at_strip_start(-124.0));
        assert!(at_strip_end(-200.0, 1000.0, 800.0));
        assert!(!at_strip_end(-100.0, 1000.0, 800.0));
        // 装得下时两端同时成立——按钮本来也不画。
        assert!(at_strip_start(0.0));
        assert!(at_strip_end(0.0, 600.0, 800.0));
    }
}
