//! 侧栏标签右键菜单（旧壳 `display::context_menu` Tab 目标）。
//!
//! 「分叉 AI 会话」必须在菜单打开当下按 hook 身份 + 官方 fork 语法重算，
//! 不能吃侧栏上一帧的快照——否则刚接到 session_id、或冷恢复种下身份后
//! 右键仍看不到这一行。

use gpui::{
    App, InteractiveElement as _, MouseButton, ParentElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::menu::PopupMenuItem;

use crate::display::color::Rgb;
use crate::gpui_shell::prelude::*;
use crate::session::LaunchSession;
use nebula_split::SplitDirection;

use super::{
    CloseActiveTerminal, NebulaWorkspace, RenameActiveTab, SplitDown, SplitRight, WorkspaceTab,
};

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
    /// 右键打开当下现查 fork 资格，并先选中该行（旧壳 chrome 同惯例）。
    pub(super) fn tab_context_menu(
        menu: PopupMenu,
        workspace: gpui::WeakEntity<Self>,
        ix: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> PopupMenu {
        let (terminal, ai_fork, color) = workspace
            .upgrade()
            .map(|entity| {
                entity.update(cx, |this, cx| {
                    if ix < this.tabs.len() {
                        this.activate_tab(ix, window, cx);
                    }
                    (
                        this.tabs.get(ix).is_some_and(WorkspaceTab::is_terminal),
                        tab_ai_fork_enabled(this, ix, cx),
                        this.meta(ix).color,
                    )
                })
            })
            .unwrap_or((false, false, None));
        Self::tab_popup_menu(
            menu.external_link_icon(false),
            workspace,
            ix,
            terminal,
            ai_fork,
            color,
        )
    }

    /// 标签行右键命令表。条目、分组与键帽对齐旧壳 Tab 目标。
    fn tab_popup_menu(
        mut menu: PopupMenu,
        workspace: gpui::WeakEntity<Self>,
        ix: usize,
        terminal: bool,
        ai_fork: bool,
        color: Option<Rgb>,
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
                                    workspace.split_focused(SplitDirection::LeftRight, window, cx);
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
                                    workspace.split_focused(SplitDirection::TopBottom, window, cx);
                                });
                            }
                        }),
                );
        }
        let rename = workspace.clone();
        let close = workspace.clone();
        menu = menu
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
