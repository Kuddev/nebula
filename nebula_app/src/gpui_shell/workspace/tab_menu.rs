//! 侧栏标签右键菜单（旧壳 `display::context_menu` Tab 目标）。
//!
//! 「分叉 AI 会话」必须在菜单打开当下按 hook 身份 + 官方 fork 语法重算，
//! 不能吃侧栏上一帧的快照——否则刚接到 session_id、或冷恢复种下身份后
//! 右键仍看不到这一行。
//!
//! # 为什么不用 `ContextMenuExt::context_menu()`
//!
//! 2026-08-18：那个扩展会把 `ContextMenu` 包在被挂载元素的**外面**，而它的
//! `ElementId` 在 fork 里是硬编码的 `"context-menu"`
//! （`crates/ui/src/menu/context_menu.rs`）。包裹层的 id 先于内层行 id 入
//! `GlobalElementId` 栈，于是每个标签行的 `ContextMenu` 算出来的 id 路径完全
//! 相同、共享同一份 element state（含那个 `Rc<RefCell<ContextMenuSharedState>>`）。
//! 菜单一打开，`open == true` 对**所有**标签行同时成立，每一行都渲染同一个
//! `PopupMenu` entity、落在同一个锚点上。面板底不透明，叠 N 次看不出来；
//! popover 阴影是半透明的，alpha 叠 N 层——这就是「标签越多，右键菜单阴影
//! 越厚」的全部原因，与打包资源和用户配置都无关。
//!
//! 所以这里跟 `file_tree.rs` 用同一套载体：标签行只记锚点和下标，菜单由
//! workspace 根上唯一一份 `deferred(anchored)` 画，组件自带的 popover 阴影
//! 因此保持单层。

use gpui::{
    AnyElement, App, Context, Corner, DismissEvent, Entity, Focusable as _,
    InteractiveElement as _, IntoElement as _, MouseButton, ParentElement as _, Pixels, Point,
    Styled as _, Subscription, Window, anchored, deferred, div, prelude::FluentBuilder as _, px,
};
use gpui_component::menu::PopupMenuItem;

use crate::display::color::Rgb;
use crate::gpui_shell::prelude::*;
use crate::session::LaunchSession;
use nebula_split::SplitDirection;

use super::{
    CloseActiveTerminal, MoveTabLeft, MoveTabRight, NebulaWorkspace, RenameActiveTab, SplitDown,
    SplitRight, WorkspaceTab,
};

/// 标签右键菜单的宿主。画在 workspace 根上，不进标签行的子孙树——理由见
/// 模块头。
pub(super) struct TabContextMenu {
    menu: Entity<PopupMenu>,
    position: Point<Pixels>,
    /// 菜单里每条命令捕获的都是下标。标签集合在菜单挂着的时候变了（远端
    /// 或 AI 侧自动关标签），这一份就不该再画。
    ix: usize,
    _subscription: Subscription,
}

/// 旧壳 `sync_chrome_tabs`：只有 Default / 检测 Shell 的 tab 才允许分叉。
/// Profile 可能直接启动 agent；SSH 会把命令打进认证提示。
pub(super) fn tab_launch_allows_ai_fork(launch: Option<&LaunchSession>) -> bool {
    matches!(launch, None | Some(LaunchSession::Default) | Some(LaunchSession::Shell { .. }))
}

pub(super) fn tab_ai_fork_enabled(workspace: &NebulaWorkspace, ix: usize, cx: &App) -> bool {
    if !tab_launch_allows_ai_fork(workspace.meta(ix).launch.as_ref()) {
        return false;
    }
    workspace
        .tabs
        .get(ix)
        .and_then(WorkspaceTab::focused_view)
        .and_then(|view| view.read(cx).ai_fork_command())
        .is_some()
}

impl NebulaWorkspace {
    /// 右键：先选中该行（旧壳 chrome 同惯例），再按当下的 hook 身份现查
    /// fork 资格，最后把菜单交给根上那一份宿主。
    pub(super) fn open_tab_context_menu(
        &mut self,
        ix: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ix >= self.tabs.len() {
            return;
        }
        self.activate_tab(ix, window, cx);
        let terminal = self.tabs.get(ix).is_some_and(WorkspaceTab::is_terminal);
        let ai_fork = tab_ai_fork_enabled(self, ix, cx);
        let color = self.meta(ix).color;
        let tab_count = self.tabs.len();
        let workspace = cx.entity().downgrade();
        let menu = PopupMenu::build(window, cx, move |menu, _window, _cx| {
            Self::tab_popup_menu(
                menu.external_link_icon(false),
                workspace,
                ix,
                terminal,
                ai_fork,
                color,
                tab_count,
            )
        });
        menu.focus_handle(cx).focus(window);
        let subscription = cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, _, cx| {
            this.tab_menu = None;
            cx.notify();
        });
        self.tab_menu = Some(TabContextMenu { menu, position, ix, _subscription: subscription });
        cx.notify();
    }

    pub(super) fn render_tab_context_menu(&self) -> Option<AnyElement> {
        let state = self.tab_menu.as_ref()?;
        if state.ix >= self.tabs.len() {
            return None;
        }
        Some(
            deferred(
                anchored()
                    .position(state.position)
                    .snap_to_window_with_margin(px(8.0))
                    .anchor(Corner::TopLeft)
                    .child(state.menu.clone()),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// 标签行右键命令表。条目、分组与键帽对齐旧壳 Tab 目标。
    #[allow(clippy::too_many_arguments)]
    fn tab_popup_menu(
        mut menu: PopupMenu,
        workspace: gpui::WeakEntity<Self>,
        ix: usize,
        terminal: bool,
        ai_fork: bool,
        color: Option<Rgb>,
        tab_count: usize,
    ) -> PopupMenu {
        if ai_fork {
            let target = workspace.clone();
            menu = menu
                .item(PopupMenuItem::new("分叉 AI 会话").icon(IconName::Bot).on_click(
                    move |_, window, cx| {
                        if let Some(workspace) = target.upgrade() {
                            workspace.update(cx, |workspace, cx| {
                                workspace.fork_ai_session(ix, window, cx);
                            });
                        }
                    },
                ))
                .separator();
        }
        if terminal {
            let duplicate = workspace.clone();
            let move_to_window = workspace.clone();
            let export = workspace.clone();
            let split_right = workspace.clone();
            let split_down = workspace.clone();
            menu = menu
                .item(PopupMenuItem::new("复制标签页").icon(IconName::Copy).on_click(
                    move |_, window, cx| {
                        if let Some(workspace) = duplicate.upgrade() {
                            workspace.update(cx, |workspace, cx| {
                                workspace.duplicate_tab(ix, window, cx);
                            });
                        }
                    },
                ))
                .item(
                    PopupMenuItem::new("移到新窗口")
                        .icon(IconName::ExternalLink)
                        .on_click(move |_, _, cx| {
                            if let Some(workspace) = move_to_window.upgrade() {
                                workspace.update(cx, |workspace, cx| {
                                    workspace.schedule_move_tab_to_new_window(ix, cx);
                                });
                            }
                        }),
                )
                .item(PopupMenuItem::new("导出为工作区…").icon(IconName::Inbox).on_click(
                    move |_, window, cx| {
                        if let Some(workspace) = export.upgrade() {
                            workspace.update(cx, |workspace, cx| {
                                workspace.export_tab(ix, window, cx);
                            });
                        }
                    },
                ))
                .separator()
                // `action` 只用来渲染键帽：handler 存在时组件不会 dispatch
                // 它（见 PopupMenu::confirm），所以命令仍然作用在 `ix` 上。
                .item(
                    PopupMenuItem::new("左右分屏")
                        .icon(IconName::PanelRight)
                        .action(Box::new(SplitRight))
                        .on_click(move |_, window, cx| {
                            if let Some(workspace) = split_right.upgrade() {
                                workspace.update(cx, |workspace, cx| {
                                    workspace.activate_tab(ix, window, cx);
                                    let _ = workspace.split_focused(
                                        SplitDirection::LeftRight,
                                        window,
                                        cx,
                                    );
                                });
                            }
                        }),
                )
                .item(
                    PopupMenuItem::new("上下分屏")
                        .icon(IconName::PanelBottom)
                        .action(Box::new(SplitDown))
                        .on_click(move |_, window, cx| {
                            if let Some(workspace) = split_down.upgrade() {
                                workspace.update(cx, |workspace, cx| {
                                    workspace.activate_tab(ix, window, cx);
                                    let _ = workspace.split_focused(
                                        SplitDirection::TopBottom,
                                        window,
                                        cx,
                                    );
                                });
                            }
                        }),
                );
        }
        let rename = workspace.clone();
        let close = workspace.clone();
        let move_left = workspace.clone();
        let move_right = workspace.clone();
        menu = menu
            .separator()
            // 位置移动：`action` 只渲键帽，命令仍作用在 `ix` 上（见上面的
            // 分屏两项）。首/末位灰掉而不是隐藏——菜单条目忽隐忽现比灰掉
            // 更难认。
            .item(
                PopupMenuItem::new("向左移动")
                    .icon(IconName::ArrowLeft)
                    .action(Box::new(MoveTabLeft))
                    .disabled(ix == 0)
                    .on_click(move |_, window, cx| {
                        if let Some(workspace) = move_left.upgrade() {
                            workspace.update(cx, |workspace, cx| {
                                workspace.activate_tab(ix, window, cx);
                                workspace.move_active_tab(false, window, cx);
                            });
                        }
                    }),
            )
            .item(
                PopupMenuItem::new("向右移动")
                    .icon(IconName::ArrowRight)
                    .action(Box::new(MoveTabRight))
                    .disabled(ix + 1 >= tab_count)
                    .on_click(move |_, window, cx| {
                        if let Some(workspace) = move_right.upgrade() {
                            workspace.update(cx, |workspace, cx| {
                                workspace.activate_tab(ix, window, cx);
                                workspace.move_active_tab(true, window, cx);
                            });
                        }
                    }),
            )
            .separator()
            .item(
                PopupMenuItem::new("重命名")
                    .icon(IconName::ALargeSmall)
                    .action(Box::new(RenameActiveTab))
                    .on_click(move |_, window, cx| {
                        if let Some(workspace) = rename.upgrade() {
                            workspace.update(cx, |workspace, cx| {
                                workspace.begin_rename(ix, window, cx);
                            });
                        }
                    }),
            )
            .item(
                PopupMenuItem::new("关闭")
                    .icon(IconName::Close)
                    .action(Box::new(CloseActiveTerminal))
                    .on_click(move |_, window, cx| {
                        if let Some(workspace) = close.upgrade() {
                            workspace.update(cx, |workspace, cx| {
                                workspace.request_close_tab(ix, window, cx);
                            });
                        }
                    }),
            );
        Self::tab_color_items(menu, workspace, ix, color)
    }

    /// 标签颜色行（旧壳菜单尾部的色板）：首槽 `A` = 无色，其后是 7 枚品牌
    /// 色。当前色带一圈选中环，再点一次同色即取消。
    fn tab_color_items(
        menu: PopupMenu,
        workspace: gpui::WeakEntity<Self>,
        ix: usize,
        current: Option<Rgb>,
    ) -> PopupMenu {
        menu.separator().item(PopupMenuItem::label("标签颜色")).item(PopupMenuItem::element(
            move |_, cx| {
                let swatches = std::iter::once(None)
                    .chain(crate::display::context_menu::TAB_COLORS.into_iter().map(Some));
                let mut row = h_flex().gap_1().py_1();
                for (slot, color) in swatches.enumerate() {
                    let selected = color == current;
                    let target = workspace.clone();
                    let fill = color
                        .map(|color| gpui::Rgba {
                            r: color.r as f32 / 255.0,
                            g: color.g as f32 / 255.0,
                            b: color.b as f32 / 255.0,
                            a: 1.0,
                        })
                        .unwrap_or_else(|| cx.theme().primary.into());
                    row = row.child(
                        div()
                            .id(("tab-color", slot))
                            .size(px(20.0))
                            .rounded(px(5.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(fill)
                            .cursor_pointer()
                            .when(selected, |swatch| {
                                swatch.border_2().border_color(cx.theme().foreground)
                            })
                            .when(color.is_none(), |swatch| {
                                swatch
                                    .text_size(px(11.0))
                                    .text_color(cx.theme().primary_foreground)
                                    .child("A")
                            })
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                if let Some(workspace) = target.upgrade() {
                                    workspace.update(cx, |workspace, cx| {
                                        let next = if selected { None } else { color };
                                        workspace.set_tab_color(ix, next, cx);
                                    });
                                }
                            }),
                    );
                }
                row
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::tab_launch_allows_ai_fork;
    use crate::session::LaunchSession;

    #[test]
    fn tab_launch_allows_ai_fork_only_for_plain_shells() {
        assert!(tab_launch_allows_ai_fork(None));
        assert!(tab_launch_allows_ai_fork(Some(&LaunchSession::Default)));
        assert!(tab_launch_allows_ai_fork(Some(&LaunchSession::Shell {
            name: "pwsh".into(),
            program: "pwsh.exe".into(),
            args: Vec::new(),
        })));
        assert!(!tab_launch_allows_ai_fork(Some(&LaunchSession::Ssh { host: "box".into() })));
        assert!(!tab_launch_allows_ai_fork(Some(&LaunchSession::Profile {
            name: "claude".into(),
            command: "claude".into(),
            args: Vec::new(),
            cwd: None,
            shell_id: None,
        })));
    }
}
