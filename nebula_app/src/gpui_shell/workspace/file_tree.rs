//! 侧栏文件树渲染（从 workspace.rs 拆出以守行数预算）。

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Anchor, AppContext as _, ClipboardItem, Context, DismissEvent, Entity, Focusable as _,
    InteractiveElement as _, IntoElement as _, MouseButton, MouseDownEvent, ParentElement as _,
    Pixels, Point, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Window, anchored, deferred, div, px,
};
use gpui_component::menu::PopupMenuItem;

use crate::gpui_shell::prelude::*;

use super::NebulaWorkspace;

/// 文件树右键菜单的宿主：必须挂在 workspace 根上，不能当抽屉行的 child。
///
/// `ContextMenuExt` 会把 `deferred(anchored(PopupMenu))` 挂回触发行；行在带
/// `shadow` 的抽屉里面。菜单翻到抽屉左缘时，抽屉投影会垫在菜单周围，看起来
/// 比侧栏 Tab 右键厚一截。
pub(super) struct FileTreeContextMenu {
    menu: Entity<PopupMenu>,
    position: Point<Pixels>,
    _subscription: Subscription,
}

/// `--cd` 只指定来宾工作目录，不指定要执行的程序；这样 WSL 仍会读取
/// `/etc/passwd` 中由 `chsh` 配置的默认 shell。
pub(super) fn wsl_terminal_launch_at(
    name: String,
    program: String,
    distro: String,
    guest_path: String,
) -> crate::session::LaunchSession {
    crate::session::LaunchSession::Shell {
        name,
        program,
        args: vec!["-d".to_owned(), distro, "--cd".to_owned(), guest_path],
    }
}

impl NebulaWorkspace {
    pub(super) fn render_file_tree(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view_switch = self.render_side_panel_switch(cx).into_any_element();
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let hover = theme.list_hover;
        let selected_bg = theme.list_active;
        // 文件树字位是内置 Nerd Font 图标；固定 Maple，不能让终端主字体
        // 改变折叠箭头和文件图标的 advance。
        let symbol_family: SharedString = crate::font_install::REQUIRED_FONT_FAMILY.into();
        let selected_path = self.side_panel.selected.clone();
        let scroll = self.side_panel.scroll;
        let root = self
            .side_panel
            .root()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "等待终端上报工作目录…".to_owned());
        let rows: Vec<_> = self.side_panel.file_rows().iter().skip(scroll).cloned().collect();
        let rows_empty = rows.is_empty();
        let snapshot_pending = self.side_panel.snapshot_pending();
        let enumeration_failed = self.side_panel.enumeration_failed();

        let row_elements = rows.into_iter().enumerate().map(|(visible_ix, row)| {
            let path = row.path.clone();
            let open_path = path.clone();
            let open_guest_path = row.guest_path.clone();
            let menu_path = path.clone();
            let guest_path = row.guest_path.clone();
            let menu_guest_path = guest_path.clone();
            let drag_guest_path = guest_path.clone();
            let drag_path = path.clone();
            let drag_name = row.name.clone();
            let is_dir = row.is_dir;
            let is_parent = row.is_parent;
            let selected = selected_path.as_ref() == Some(&path);
            let fg = if row.ignored { muted } else { theme.foreground };
            let _chevron = if row.is_dir && !row.is_parent {
                if row.expanded { "⌄" } else { "›" }
            } else {
                ""
            };
            let file_glyph =
                (!row.is_dir).then(|| crate::display::side_panel::file_type_icon(&row.name));
            let legacy_chevron = row
                .is_dir
                .then(|| {
                    (!row.is_parent).then(|| crate::display::side_panel::chevron_icon(row.expanded))
                })
                .flatten();

            h_flex()
                .id(SharedString::from(format!("file-tree-row-{visible_ix}")))
                .h(px(30.0))
                .w_full()
                .items_center()
                .pr_2()
                .pl(px(8.0 + row.depth as f32 * 16.0))
                .gap_1()
                .rounded_md()
                .text_color(fg)
                .when(selected, |item| item.bg(selected_bg))
                .hover(|item| item.bg(hover))
                .child(
                    div()
                        .w(px(12.0))
                        .flex_shrink_0()
                        .font_family(symbol_family.clone())
                        .text_sm()
                        .text_color(muted)
                        .child(legacy_chevron.unwrap_or("")),
                )
                .when(is_dir, |item| {
                    item.child(
                        div()
                            .w(px(16.0))
                            .flex_shrink_0()
                            .font_family(symbol_family.clone())
                            .text_sm()
                            .text_color(if row.ignored { muted } else { theme.foreground })
                            .child(crate::display::side_panel::folder_icon(row.expanded)),
                    )
                })
                .when_some(file_glyph, |item, glyph| {
                    item.child(
                        div()
                            .w(px(16.0))
                            .flex_shrink_0()
                            .font_family(symbol_family.clone())
                            .text_sm()
                            .child(glyph),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(row.name),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if is_dir {
                        if this.side_panel.click_row(visible_ix) {
                            cx.notify();
                        }
                    } else {
                        this.side_panel.selected = Some(path.clone());
                        cx.notify();
                    }
                }))
                .when(!is_dir && !is_parent, |item| {
                    item.on_double_click(cx.listener(move |this, _, window, cx| {
                        // 与旧壳 chrome 同合同：应用内能读的（图片/可读文本）
                        // 开查看 tab，其余交系统处理器。WSL 行先映射为官方 UNC
                        // 形式，不能把 `/home/...` 直接交给 Windows 文件 API。
                        if let Some(guest) = open_guest_path.clone() {
                            this.open_wsl_document_path(guest, window, cx);
                        } else {
                            this.open_document_path(open_path.clone(), window, cx);
                        }
                    }))
                })
                .when(!is_parent, |item| {
                    // 拖到终端 = 把路径粘进 shell（旧壳 `FileDrag` 同一合同）。
                    // WSL 行拖的是**来宾**路径：宿主那份拼写只是展开用的键，
                    // 在来宾的 shell 里不存在。
                    let drag = crate::gpui_shell::file_drop::FileTreeDrag {
                        path_text: drag_guest_path
                            .clone()
                            .unwrap_or_else(|| drag_path.display().to_string()),
                        name: drag_name.clone(),
                    };
                    item.on_drag(drag, |drag, _offset, _window, cx| {
                        cx.new(|_| {
                            crate::gpui_shell::file_drop::FileDragGhost::new(drag.name.clone())
                        })
                    })
                })
                .when(!is_parent, |item| {
                    // 不在抽屉行上挂 `.context_menu()`：那会把 PopupMenu 挂回
                    // 行的 child，菜单仍是带投影抽屉的子孙。右键只记锚点，
                    // 菜单由 workspace 根上的 `deferred(anchored)` 画。
                    item.on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_file_tree_context_menu(
                                menu_path.clone(),
                                menu_guest_path.clone(),
                                is_dir,
                                event.position,
                                window,
                                cx,
                            );
                        }),
                    )
                })
        });

        v_flex()
            .h_full()
            .w(px(320.0))
            .flex_shrink_0()
            .my_2()
            .mr_2()
            .p_2()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            // 抽屉在窗口右侧；右键菜单常翻到左缘。Tailwind `shadow_lg`
            //（10px 下偏移 + 15px 模糊，再加一层）会垫在菜单周围，比 Tab
            // 右键的 `popover_shadow` 厚一截。抽屉本身也是 popover 面，跟
            // 菜单用同一套紧凑投影。
            .shadow(gpui_component::popover_shadow(theme.is_dark()))
            .occlude()
            .child(view_switch)
            .child(
                h_flex()
                    .h(px(30.0))
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(muted)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(root),
                    )
                    .child(
                        Button::new("file-tree-refresh")
                            .icon(IconName::Redo2)
                            .ghost()
                            .xsmall()
                            .tooltip("刷新目录树")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.side_panel.request_refresh();
                                // 刷新同时恢复“跟随当前 pane”；否则点过 `..` 后
                                // custom root 会继续压住当前终端，按钮只会刷新旧目录。
                                this.sync_side_panel_to_active(true, cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("file-tree-close")
                            .icon(IconName::Close)
                            .ghost()
                            .xsmall()
                            .tooltip("关闭目录树 (Ctrl+Shift+F)")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_file_tree(cx);
                            })),
                    ),
            )
            .when_some(self.side_panel.root_notice(), |panel, notice| {
                panel.child(div().text_xs().text_color(theme.warning).child(notice.to_owned()))
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(
                        v_flex()
                            .w_full()
                            .gap_1()
                            .children(row_elements)
                            .when(rows_empty, |list| {
                                list.child(
                                    div()
                                        .w_full()
                                        .py_4()
                                        .px_2()
                                        .text_center()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(if snapshot_pending {
                                            "正在读取目录…"
                                        } else if enumeration_failed {
                                            // 读不到 ≠ 目录是空的。WSL 冷启动可能
                                            // 耗尽预算，那时必须给可重试的提示。
                                            "读取目录失败，点上方刷新重试"
                                        } else {
                                            "此目录为空"
                                        }),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }

    fn open_file_tree_context_menu(
        &mut self,
        path: PathBuf,
        guest_path: Option<String>,
        is_dir: bool,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.side_panel.selected = Some(path.clone());
        let wsl_distro = guest_path
            .as_ref()
            .and_then(|_| self.side_panel.file_wsl_root().map(|root| root.distro.clone()));
        let workspace = cx.entity().downgrade();
        let menu = PopupMenu::build(window, cx, move |menu, window, _cx| {
            file_tree_popup_menu(
                menu.external_link_icon(false),
                workspace,
                path,
                guest_path,
                wsl_distro,
                is_dir,
                window,
            )
        });
        menu.focus_handle(cx).focus(window, cx);
        let subscription = cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, _, cx| {
            this.file_tree_menu = None;
            cx.notify();
        });
        self.file_tree_menu =
            Some(FileTreeContextMenu { menu, position, _subscription: subscription });
        cx.notify();
    }

    pub(super) fn render_file_tree_context_menu(&self) -> Option<gpui::AnyElement> {
        let state = self.file_tree_menu.as_ref()?;
        Some(
            deferred(
                anchored()
                    .position(state.position)
                    .snap_to_window_with_margin(px(8.0))
                    .anchor(Anchor::TopLeft)
                    .child(state.menu.clone()),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// WSL 文件树行只携带来宾路径；发行版身份来自当前树根。通过
    /// `\\wsl.localhost` 交给现有文档/系统打开路由，保持文件类型行为一致。
    fn open_wsl_document_path(
        &mut self,
        guest_path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.side_panel.file_wsl_root() else { return };
        let path = crate::shell_detect::wsl_unc_path(&root.distro, &guest_path);
        self.open_document_path(path, window, cx);
    }

    /// `wsl.exe --cd` 不执行指定命令，因此仍由来宾 `/etc/passwd` 选择 zsh/fish；
    /// 这与普通 WSL 新标签相同，并避免重引入 v1.1.0 的 `--exec bash` 回归。
    fn add_wsl_terminal_at(
        &mut self,
        distro: String,
        guest_path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let detected = crate::shell_detect::detect_shells().into_iter().find(|shell| {
            crate::shell_detect::wsl_launch_distro(&shell.program, &shell.args)
                .is_some_and(|name| name.eq_ignore_ascii_case(&distro))
        });
        let (name, program) = detected
            .map(|shell| (shell.name, shell.program))
            .or_else(|| match self.meta(self.active).launch {
                Some(crate::session::LaunchSession::Shell { name, program, args })
                    if crate::shell_detect::wsl_launch_distro(&program, &args)
                        .is_some_and(|name| name.eq_ignore_ascii_case(&distro)) =>
                {
                    Some((name, program))
                },
                _ => None,
            })
            .unwrap_or_else(|| (format!("WSL · {distro}"), "wsl.exe".to_owned()));
        let launch = wsl_terminal_launch_at(name, program, distro, guest_path);
        self.add_terminal_with(launch, None, None, window, cx);
    }

    fn request_delete_file_tree_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !path.exists() {
            self.side_panel.set_notice("路径已不存在".to_owned());
            cx.notify();
            return;
        }
        let is_dir = path.is_dir();
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let title: SharedString =
            format!("删除 {}？", crate::display::truncate_tab_label(&name, 28)).into();
        let body: SharedString = if is_dir {
            "文件夹及其全部内容会移入回收站。".into()
        } else {
            "文件会移入回收站。".into()
        };
        let workspace = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, _cx| {
            let workspace = workspace.clone();
            let path = path.clone();
            center_confirm_dialog(dialog, window)
                .title(title.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("删除")
                        .ok_variant(gpui_component::button::ButtonVariant::Danger)
                        .cancel_text("取消")
                        .show_cancel(true),
                )
                .child(body.clone())
                .on_ok(move |_, _, cx| {
                    let _ = workspace.update(cx, |this, cx| {
                        match crate::display::send_to_recycle_bin(&path) {
                            Ok(()) => {
                                this.side_panel.request_refresh();
                                this.sync_side_panel_to_active(false, cx);
                            },
                            Err(error) => this.side_panel.set_notice(format!("删除失败：{error}")),
                        }
                        cx.notify();
                    });
                    true
                })
        });
    }
}

fn file_tree_popup_menu(
    menu: PopupMenu,
    workspace: gpui::WeakEntity<NebulaWorkspace>,
    path: PathBuf,
    guest_path: Option<String>,
    wsl_distro: Option<String>,
    is_dir: bool,
    _window: &mut Window,
) -> PopupMenu {
    if let Some(guest_path) = guest_path {
        let copy_guest = guest_path.clone();
        let copy =
            PopupMenuItem::new("复制 Linux 路径").icon(IconName::Copy).on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy_guest.clone()));
            });
        let Some(distro) = wsl_distro else { return menu.item(copy) };
        let first = if is_dir {
            let open_here = workspace.clone();
            let open_distro = distro.clone();
            let open_guest = guest_path.clone();
            PopupMenuItem::new("在此处打开终端").icon(IconName::SquareTerminal).on_click(
                move |_, window, cx| {
                    if let Some(workspace) = open_here.upgrade() {
                        workspace.update(cx, |this, cx| {
                            this.add_wsl_terminal_at(
                                open_distro.clone(),
                                open_guest.clone(),
                                window,
                                cx,
                            );
                        });
                    }
                },
            )
        } else {
            let open = workspace.clone();
            let open_guest = guest_path.clone();
            PopupMenuItem::new("打开").icon(IconName::File).on_click(move |_, window, cx| {
                if let Some(workspace) = open.upgrade() {
                    workspace.update(cx, |this, cx| {
                        this.open_wsl_document_path(open_guest.clone(), window, cx);
                    });
                }
            })
        };
        let reveal_path = crate::shell_detect::wsl_unc_path(&distro, &guest_path);
        return menu
            .item(first)
            .item(PopupMenuItem::new("在资源管理器中显示").icon(IconName::FolderOpen).on_click(
                move |_, _, _| {
                    super::reveal_in_file_manager(&reveal_path);
                },
            ))
            .item(copy);
    }
    let first = if is_dir {
        let open_here = workspace.clone();
        let dir = path.clone();
        PopupMenuItem::new("在此处打开终端").icon(IconName::SquareTerminal).on_click(
            move |_, window, cx| {
                if let Some(workspace) = open_here.upgrade() {
                    workspace.update(cx, |this, cx| {
                        this.add_terminal_at(Some(dir.clone()), None, window, cx);
                    });
                }
            },
        )
    } else {
        let open = workspace.clone();
        let file = path.clone();
        PopupMenuItem::new("打开").icon(IconName::File).on_click(move |_, window, cx| {
            if let Some(workspace) = open.upgrade() {
                workspace.update(cx, |this, cx| {
                    this.open_document_path(file.clone(), window, cx);
                });
            }
        })
    };
    let reveal_path = path.clone();
    let copy_path = path.clone();
    let delete = workspace;
    menu.item(first)
        .item(PopupMenuItem::new("在资源管理器中显示").icon(IconName::FolderOpen).on_click(
            move |_, _, _| {
                super::reveal_in_file_manager(&reveal_path);
            },
        ))
        .item(PopupMenuItem::new("复制路径").icon(IconName::Copy).on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy_path.display().to_string()));
        }))
        .separator()
        .item(PopupMenuItem::new("删除").icon(IconName::Delete).on_click(move |_, window, cx| {
            if let Some(workspace) = delete.upgrade() {
                workspace.update(cx, |this, cx| {
                    this.request_delete_file_tree_path(path.clone(), window, cx);
                });
            }
        }))
}
