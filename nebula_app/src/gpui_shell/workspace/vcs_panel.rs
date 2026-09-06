//! VCS 抽屉页：视图切换钮 + Git/SVN 变更列表与操作条。
//!
//! 2026-08-31 从 `workspace.rs` 整块搬出（那边已到 5517 行，远超行数预算，
//! 而 VCS 面板正要长出 SVN 的一批操作）。这里刻意用 `use super::*` 而不是
//! 逐项 import：搬迁是纯位移，父模块的 import 面照旧就不会漏名字，也不会
//! 在搬的过程中悄悄改掉某个 trait 的引入方式。

use super::*;
use gpui_component::menu::PopupMenuItem;

impl NebulaWorkspace {
    pub(super) fn render_side_panel_switch(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::display::side_panel::PanelView;

        let files = self.side_panel.view == PanelView::Files;
        let git = self.side_panel.view == PanelView::Git;
        let git_count = self
            .side_panel
            .git()
            .map(|snapshot| snapshot.unstaged.len() + snapshot.staged.len())
            .unwrap_or(0);
        let vcs_name = match self.side_panel.vcs() {
            Some(crate::display::side_panel::VcsKind::Svn)
            | Some(crate::display::side_panel::VcsKind::SvnRepository) => "SVN",
            _ => "Git",
        };
        let is_git_vcs = vcs_name == "Git";
        h_flex()
            .gap_1()
            .child(
                Button::new("side-panel-files")
                    .icon(IconName::FolderClosed)
                    .label("文件")
                    .small()
                    .selected(files)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.select_side_panel_view(PanelView::Files, cx);
                    })),
            )
            .child(
                Button::new("side-panel-git")
                    .label(vcs_name)
                    .when(is_git_vcs, |button| button.icon(IconName::Github))
                    .small()
                    .selected(git)
                    .when(git_count > 0, |button| button.label(format!("{vcs_name} {git_count}")))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.select_side_panel_view(PanelView::Git, cx);
                    })),
            )
    }

    pub(super) fn render_git_tree(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view_switch = self.render_side_panel_switch(cx).into_any_element();
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let hover = theme.list_hover;
        let selected_bg = theme.list_active;
        // 第三条轨道固定使用从主题主色旋出的紫色。普通 lane 不能借 danger
        // 红色（那会误报错误），也不能直接借 link（有些主题里与 primary 同色）。
        let lane_purple = gpui::Hsla {
            h: (theme.primary.h + 0.20) % 1.0,
            s: theme.primary.s.max(0.42),
            l: theme.primary.l,
            a: theme.primary.a,
        };
        let symbol: SharedString = crate::font_install::REQUIRED_FONT_FAMILY.into();
        // VCS 状态跟着 `SidePanel::vcs_root`：显式浏览定位优先，其次终端 cwd。
        let root = self.side_panel.vcs_root().map(Path::to_path_buf);
        let selected = self.side_panel.selected.clone();
        let git = self.side_panel.git().cloned();
        let vcs = git.as_ref().map(|info| info.vcs);
        let git_view = self.side_panel.git_view;
        let op_running = self.side_panel.op_running();
        let op_error = self.side_panel.op_error();
        // 浏览定位是否生效——决定要不要画"回到终端当前目录"。链式构建里不能
        // 再借 `self`，所以这些和下面的弱引用都在这里一次取好。
        let browsing_elsewhere = self.side_panel.custom_root_active();
        // 下拉菜单的动作要在自己的闭包里回到本实体；`cx.listener` 只能给
        // 直接挂在元素上的回调用，菜单项拿不到它。
        let menu_target = cx.entity().downgrade();
        let mut rows = Vec::new();

        if let Some(info) = git.as_ref() {
            use crate::display::side_panel::{GitPanelView, VcsKind};
            /// 分组决定行内操作（VS Code 的 SCM 行合同）。
            #[derive(Clone, Copy, PartialEq)]
            enum RowOps {
                /// 变更组：暂存 + 丢弃（untracked 不给丢弃——restore 不删新文件）。
                Unstaged,
                /// 已暂存组：取消暂存。
                Staged,
                /// Git 冲突组：打开工作区里的三栏合并 Tab。
                Conflict,
                /// SVN 无暂存区，具体操作由状态字母决定。
                Svn,
            }
            let is_git = info.vcs == VcsKind::Git;
            if is_git && git_view == GitPanelView::History {
                let graph_rows = git_graph_rows(&info.history);
                let (lane_width, lane_spacing) = git_lane_layout(&graph_rows);
                if info.history.is_empty() {
                    rows.push(
                        div()
                            .py_3()
                            .px_2()
                            .text_sm()
                            .text_color(muted)
                            .child("还没有提交历史")
                            .into_any_element(),
                    );
                }
                for (index, (commit, graph)) in
                    info.history.iter().zip(graph_rows.into_iter()).enumerate()
                {
                    let graph_cell = git_lane_canvas(
                        graph,
                        lane_width,
                        lane_spacing,
                        [theme.primary, theme.success, lane_purple, theme.warning],
                        theme.popover,
                    );
                    let refs = git_ref_labels(&commit.decorations)
                        .into_iter()
                        .take(2)
                        .map(|git_ref| {
                            let color = match git_ref.kind {
                                GitRefKind::Head | GitRefKind::Local => theme.primary,
                                GitRefKind::Remote => lane_purple,
                                GitRefKind::Tag => theme.warning,
                            };
                            div()
                                .h(px(15.0))
                                .max_w(px(72.0))
                                .flex_shrink_0()
                                .px(px(4.0))
                                .rounded(px(4.0))
                                .border_1()
                                .border_color(color.opacity(0.38))
                                .bg(color.opacity(0.12))
                                .truncate()
                                .text_size(px(9.5))
                                .text_color(color)
                                .child(git_ref.label)
                                .into_any_element()
                        })
                        .collect::<Vec<_>>();
                    let meta = format!(
                        "{} · {} · {}",
                        commit.author,
                        git_relative_time(commit.timestamp),
                        commit.short_hash
                    );
                    rows.push(
                        h_flex()
                            .id(SharedString::from(format!("git-history-{index}")))
                            .w_full()
                            .h(px(46.0))
                            .px_2()
                            .items_start()
                            .hover(|row| row.bg(hover))
                            .child(graph_cell)
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .justify_center()
                                    .gap_1()
                                    .child(
                                        h_flex().min_w_0().gap(px(4.0)).children(refs).child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_sm()
                                                .child(commit.subject.clone()),
                                        ),
                                    )
                                    .child(
                                        div().truncate().text_xs().text_color(muted).child(meta),
                                    ),
                            )
                            .into_any_element(),
                    );
                }
            } else {
                let conflict_paths: std::collections::HashSet<&str> =
                    info.conflicts.iter().map(|(_, path)| path.as_str()).collect();
                // 变更页保留三组；冲突页只列冲突。路径从后两组过滤，因为数据层
                // 为旧壳兼容仍把冲突同时留在 staged/unstaged。
                let mut sections: Vec<(&str, Vec<&(char, String)>, RowOps)> = Vec::new();
                if !info.conflicts.is_empty() {
                    sections.push((
                        "合并冲突",
                        info.conflicts.iter().collect(),
                        if is_git { RowOps::Conflict } else { RowOps::Svn },
                    ));
                }
                let not_conflicted =
                    |(_, path): &&(char, String)| !conflict_paths.contains(path.as_str());
                match info.vcs {
                    VcsKind::Git if git_view == GitPanelView::Changes => {
                        sections.push((
                            "已暂存",
                            info.staged.iter().filter(not_conflicted).collect(),
                            RowOps::Staged,
                        ));
                        sections.push((
                            "变更",
                            info.unstaged.iter().filter(not_conflicted).collect(),
                            RowOps::Unstaged,
                        ));
                    },
                    VcsKind::Git => {},
                    VcsKind::Svn => sections.push((
                        "修改",
                        info.unstaged.iter().filter(not_conflicted).collect(),
                        RowOps::Svn,
                    )),
                    VcsKind::SvnRepository => {},
                }
                let clean = sections.iter().all(|(_, entries, _)| entries.is_empty());
                if clean && info.vcs != VcsKind::SvnRepository {
                    rows.push(
                        div()
                            .py_2()
                            .px_2()
                            .text_sm()
                            .text_color(muted)
                            .child(if is_git && git_view == GitPanelView::Conflicts {
                                "没有待解决的冲突"
                            } else {
                                "没有更改"
                            })
                            .into_any_element(),
                    );
                }
                let discard_confirm = self.vcs_discard_confirm.clone();
                for (section, entries, ops) in sections {
                    if entries.is_empty() {
                        continue;
                    }
                    rows.push(
                        h_flex()
                            .h(px(26.0))
                            .px_2()
                            .items_center()
                            .text_xs()
                            .text_color(muted)
                            .child(section)
                            .child(div().ml_2().child(entries.len().to_string()))
                            .into_any_element(),
                    );
                    for (index, (status, relative_path)) in entries.into_iter().enumerate() {
                        let path = root
                            .as_ref()
                            .map(|root| root.join(relative_path))
                            .unwrap_or_else(|| std::path::PathBuf::from(relative_path));
                        let selected_row = selected.as_ref() == Some(&path);
                        let status_color = match status {
                            'A' | '?' => theme.success,
                            'D' | '!' => theme.danger,
                            'C' | 'U' => theme.danger,
                            _ => theme.warning,
                        };
                        // VS Code 式路径拆分：文件名主体 + 灰色父目录。
                        let (file_name, parent) = match relative_path.rfind('/') {
                            Some(pos) => (
                                relative_path[pos + 1..].to_owned(),
                                relative_path[..pos].to_owned(),
                            ),
                            None => (relative_path.clone(), String::new()),
                        };
                        let row_group =
                            SharedString::from(format!("vcs-row-actions-{section}-{index}"));
                        let open_path = path.clone();
                        let stage_path = relative_path.clone();
                        let svn_add_path = relative_path.clone();
                        let unstage_path = relative_path.clone();
                        let discard_path = relative_path.clone();
                        let resolve_path = relative_path.clone();
                        let diff_path = relative_path.clone();
                        let menu_path = relative_path.clone();
                        let merge_button_path = relative_path.clone();
                        let merge_open_path = relative_path.clone();
                        let discard_armed =
                            discard_confirm.as_deref() == Some(relative_path.as_str());
                        let git_discard = ops == RowOps::Unstaged && *status != '?';
                        let svn_revert = ops == RowOps::Svn && !matches!(*status, '?' | 'C');
                        let can_discard = git_discard || svn_revert;
                        let svn_add = ops == RowOps::Svn && *status == '?';
                        let svn_resolve = ops == RowOps::Svn && *status == 'C';
                        let svn_diff = ops == RowOps::Svn && !matches!(*status, '?' | '!');
                        rows.push(
                            h_flex()
                                .id(SharedString::from(format!(
                                    "git-tree-row-{section}-{index}-{relative_path}"
                                )))
                                .group(row_group.clone())
                                .h(px(30.0))
                                .w_full()
                                .px_2()
                                .gap_2()
                                .items_center()
                                .rounded_md()
                                .when(selected_row, |row| row.bg(selected_bg))
                                .hover(|row| row.bg(hover))
                                .child(
                                    div()
                                        .w(px(14.0))
                                        .flex_shrink_0()
                                        .font_family(symbol.clone())
                                        .text_sm()
                                        .text_color(status_color)
                                        .child(status.to_string()),
                                )
                                .child(
                                    h_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_1()
                                        .items_center()
                                        .child(
                                            div()
                                                .flex_shrink_0()
                                                .text_sm()
                                                .child(file_name.clone()),
                                        )
                                        .when(!parent.is_empty(), |line| {
                                            line.child(
                                                div()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .child(parent.clone()),
                                            )
                                        }),
                                )
                                .when(can_discard, |row| {
                                    row.child(
                                        Button::new(SharedString::from(format!(
                                            "vcs-discard-{section}-{index}"
                                        )))
                                        .map(|button| {
                                            if discard_armed {
                                                button
                                                    .label(if svn_revert {
                                                        "确认还原"
                                                    } else {
                                                        "确认丢弃"
                                                    })
                                                    .danger()
                                                    .xsmall()
                                            } else {
                                                button
                                                    .icon(IconName::Undo2)
                                                    .ghost()
                                                    .xsmall()
                                                    .tooltip(if svn_revert {
                                                        "还原 SVN 改动"
                                                    } else {
                                                        "丢弃改动"
                                                    })
                                            }
                                        })
                                        .when(!discard_armed, |button| {
                                            button
                                                .invisible()
                                                .group_hover(row_group.clone(), |button| {
                                                    button.visible()
                                                })
                                        })
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                if this.vcs_discard_confirm.as_deref()
                                                    == Some(discard_path.as_str())
                                                {
                                                    this.vcs_discard_confirm = None;
                                                    if svn_revert {
                                                        this.side_panel
                                                            .svn_revert_path(&discard_path);
                                                    } else {
                                                        this.side_panel
                                                            .git_discard_path(&discard_path);
                                                    }
                                                } else {
                                                    this.vcs_discard_confirm =
                                                        Some(discard_path.clone());
                                                }
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                })
                                .when(ops == RowOps::Unstaged && is_git, |row| {
                                    row.child(
                                        Button::new(SharedString::from(format!(
                                            "vcs-stage-{section}-{index}"
                                        )))
                                        .icon(IconName::Plus)
                                        .ghost()
                                        .xsmall()
                                        .tooltip("暂存")
                                        .invisible()
                                        .group_hover(row_group.clone(), |button| button.visible())
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.vcs_discard_confirm = None;
                                                this.side_panel.git_stage_path(&stage_path);
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                })
                                .when(svn_add, |row| {
                                    row.child(
                                        Button::new(SharedString::from(format!(
                                            "svn-add-{section}-{index}"
                                        )))
                                        .icon(IconName::Plus)
                                        .ghost()
                                        .xsmall()
                                        .tooltip("添加到 SVN")
                                        .invisible()
                                        .group_hover(row_group.clone(), |button| button.visible())
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.vcs_discard_confirm = None;
                                                this.side_panel.svn_add_path(&svn_add_path);
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                })
                                .when(svn_resolve, |row| {
                                    row.child(
                                        Button::new(SharedString::from(format!(
                                            "svn-resolve-{section}-{index}"
                                        )))
                                        .label("解决")
                                        .ghost()
                                        .xsmall()
                                        .tooltip("保留当前内容并标记冲突已解决")
                                        .invisible()
                                        .group_hover(row_group.clone(), |button| button.visible())
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.vcs_discard_confirm = None;
                                                this.side_panel.svn_resolve_path(&resolve_path);
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                })
                                .when(ops == RowOps::Svn, |row| {
                                    // 这一行的完整 SVN 操作集（日志、blame、锁、
                                    // 忽略、改名、删除、冲突、属性）都在这个菜单里，
                                    // 行内只多一个 ⋯ 位。
                                    row.child(Self::svn_row_menu(
                                        &menu_target,
                                        &menu_path,
                                        index,
                                        section,
                                    ))
                                })
                                .when(ops == RowOps::Staged, |row| {
                                    row.child(
                                        Button::new(SharedString::from(format!(
                                            "vcs-unstage-{section}-{index}"
                                        )))
                                        .icon(IconName::Minus)
                                        .ghost()
                                        .xsmall()
                                        .tooltip("取消暂存")
                                        .invisible()
                                        .group_hover(row_group.clone(), |button| button.visible())
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.vcs_discard_confirm = None;
                                                this.side_panel.git_unstage_path(&unstage_path);
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                })
                                .when(ops == RowOps::Conflict && is_git, |row| {
                                    row.child(
                                        Button::new(SharedString::from(format!(
                                            "git-resolve-{section}-{index}"
                                        )))
                                        .icon(
                                            Icon::new(Icon::empty())
                                                .path(crate::gpui_shell::assets::nav::VCS_CONFLICT),
                                        )
                                        .ghost()
                                        .xsmall()
                                        .tooltip("在三栏合并器中解决")
                                        .on_click(
                                            cx.listener(move |this, _, window, cx| {
                                                this.open_git_merge_tab(
                                                    merge_button_path.clone(),
                                                    window,
                                                    cx,
                                                );
                                            }),
                                        ),
                                    )
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.side_panel.selected = Some(path.clone());
                                    cx.notify();
                                }))
                                .on_double_click(cx.listener(move |this, _, window, cx| {
                                    if ops == RowOps::Conflict && is_git {
                                        this.open_git_merge_tab(
                                            merge_open_path.clone(),
                                            window,
                                            cx,
                                        );
                                    } else if !svn_diff
                                        || !this.side_panel.svn_diff_path(&diff_path)
                                    {
                                        // Git 与未版本化 SVN 文件仍走现有文档路由。
                                        this.open_document_path(open_path.clone(), window, cx);
                                    }
                                }))
                                .into_any_element(),
                        );
                    }
                }
            }
        }

        use crate::display::side_panel::{GitPanelView, VcsKind};
        let summary = git.as_ref().map(|info| {
            h_flex()
                .h(px(30.0))
                .items_center()
                .gap_2()
                .child(div().font_family(symbol).text_sm().text_color(muted).child("\u{ea68}"))
                .child(div().flex_1().min_w_0().text_sm().truncate().child(
                    if info.branch.is_empty() {
                        "(no branch)".to_owned()
                    } else {
                        info.branch.clone()
                    },
                ))
                .when(info.vcs != VcsKind::SvnRepository && info.ahead > 0, |row| {
                    row.child(
                        div().text_xs().text_color(theme.primary).child(format!("↑{}", info.ahead)),
                    )
                })
                .when(info.vcs != VcsKind::SvnRepository, |row| {
                    row.child(
                        div().text_xs().text_color(theme.success).child(format!("+{}", info.plus)),
                    )
                    .child(
                        div().text_xs().text_color(theme.danger).child(format!("−{}", info.minus)),
                    )
                })
        });

        // 三个入口只显示图形，语义由各自图标和 tooltip 共同承担。每个按钮
        // `flex_1`，以后追加第四个入口时仍会自动等分，不引入写死宽度。
        let git_nav = git.as_ref().filter(|info| info.vcs == VcsKind::Git).map(|_| {
            h_flex()
                .w_full()
                .gap_1()
                .child(
                    Button::new("git-view-changes")
                        .icon(
                            Icon::new(Icon::empty())
                                .path(crate::gpui_shell::assets::nav::VCS_CHANGES),
                        )
                        .flex_1()
                        .ghost()
                        .small()
                        .when(git_view == GitPanelView::Changes, |button| button.label("变更"))
                        .selected(git_view == GitPanelView::Changes)
                        .tooltip("提交 / 变更")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.side_panel.select_git_view(GitPanelView::Changes);
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("git-view-history")
                        .icon(
                            Icon::new(Icon::empty())
                                .path(crate::gpui_shell::assets::nav::VCS_HISTORY),
                        )
                        .flex_1()
                        .ghost()
                        .small()
                        .when(git_view == GitPanelView::History, |button| button.label("线路"))
                        .selected(git_view == GitPanelView::History)
                        .tooltip("历史线路")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.side_panel.select_git_view(GitPanelView::History);
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("git-view-conflicts")
                        .icon(
                            Icon::new(Icon::empty())
                                .path(crate::gpui_shell::assets::nav::VCS_CONFLICT),
                        )
                        .flex_1()
                        .ghost()
                        .small()
                        .when(git_view == GitPanelView::Conflicts, |button| button.label("冲突"))
                        .selected(git_view == GitPanelView::Conflicts)
                        .tooltip("解决冲突")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.side_panel.select_git_view(GitPanelView::Conflicts);
                            cx.notify();
                        })),
                )
        });
        let history_head = git
            .as_ref()
            .filter(|info| info.vcs == VcsKind::Git && git_view == GitPanelView::History)
            .map(|info| {
                h_flex()
                    .h(px(30.0))
                    .flex_shrink_0()
                    .px(px(6.0))
                    .items_center()
                    .child(div().flex_1().min_w_0().text_sm().font_semibold().child("历史线路"))
                    .child(div().text_xs().text_color(muted).child(info.history.len().to_string()))
            });

        // 服务端版本库的摘要。此前这里只有一句"需要先检出"——而 HEAD 修订号、
        // UUID、格式号、体积、最后一条提交、有没有 trunk/branches/tags，全都
        // 躺在版本库 `db/` 下的纯文本文件里，读它们连 svn 客户端都不需要。
        let repository_notice =
            git.as_ref().filter(|info| info.vcs == VcsKind::SvnRepository).map(|info| {
                let path = info
                    .repository_root
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                let summary = info.repository.clone().unwrap_or_default();
                let layout = if summary.top_level.is_empty() {
                    "空版本库 · 还没有内容".to_owned()
                } else if summary.has_standard_layout() {
                    format!("标准布局 · {}", summary.top_level.join(" / "))
                } else {
                    format!("顶层 · {}", summary.top_level.join(" / "))
                };
                let last_commit = summary.last_author.as_ref().map(|author| {
                    let date = summary
                        .last_date
                        .as_deref()
                        .and_then(|value| value.split('T').next())
                        .unwrap_or("");
                    let head = summary.head.unwrap_or_default();
                    format!("r{head} · {author} · {date}")
                });
                // 格式号和 UUID 是排障用的（"我连的到底是不是这个库"），压成一行小字。
                let mut facts = Vec::new();
                if let Some(uuid) = summary.uuid.as_deref() {
                    facts.push(format!("UUID {uuid}"));
                }
                if let Some(format) = summary.fs_format {
                    facts.push(format!("FSFS {format}"));
                }
                facts.push(human_size(summary.size_bytes));
                v_flex()
                    .gap_1()
                    .text_sm()
                    .text_color(muted)
                    .child(div().text_xs().truncate().child(path))
                    .child(layout)
                    .when_some(last_commit, |panel, line| {
                        panel.child(div().text_xs().truncate().child(line))
                    })
                    .when_some(summary.last_log.clone(), |panel, log| {
                        panel.child(
                            div().text_xs().truncate().text_color(theme.foreground).child(log),
                        )
                    })
                    .child(div().text_xs().truncate().child(facts.join(" · ")))
                    .child(
                        div()
                            .text_xs()
                            .child("服务端版本库本身没有文件状态，检出成工作副本后才有。"),
                    )
            });

        // Git 保留暂存/拉取/推送；SVN 显式提供添加/更新/日志/清理。
        // 服务端仓库只有浏览与检出，避免把无效工作副本命令暴露给用户。
        let (unstaged_len, staged_len, ahead) = git
            .as_ref()
            .map(|info| (info.unstaged.len(), info.staged.len(), info.ahead))
            .unwrap_or((0, 0, 0));
        let commit_ready = !op_running
            && git.as_ref().is_some_and(|info| match info.vcs {
                VcsKind::Git => staged_len > 0,
                VcsKind::Svn => info.svn_commit_ready(),
                VcsKind::SvnRepository => false,
            });
        let svn_add_ready = git.as_ref().is_some_and(|info| info.svn_add_ready());
        let standard_layout_done = git
            .as_ref()
            .and_then(|info| info.repository.as_ref())
            .is_some_and(|summary| summary.has_standard_layout());
        let commit_row = git
            .as_ref()
            .filter(|info| {
                info.vcs != VcsKind::SvnRepository
                    && (info.vcs != VcsKind::Git || git_view == GitPanelView::Changes)
            })
            .map(|_| {
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.git_commit_input)))
                    .child(
                        Button::new("vcs-commit")
                            .label("提交")
                            .small()
                            .disabled(!commit_ready)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.submit_vcs_commit(window, cx);
                            })),
                    )
            });
        let action_strip = if vcs == Some(VcsKind::Git) && git_view != GitPanelView::Changes {
            None
        } else {
            match vcs {
                Some(VcsKind::Git) => Some(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(
                            Button::new("git-stage-all")
                                .label("全部暂存")
                                .small()
                                .disabled(op_running || unstaged_len == 0)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.side_panel.git_stage_all();
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("git-pull")
                                .label("拉取")
                                .small()
                                .disabled(op_running)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.side_panel.git_pull();
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("git-push")
                                .label(if ahead > 0 {
                                    SharedString::from(format!("推送 ↑{ahead}"))
                                } else {
                                    SharedString::from("推送")
                                })
                                .small()
                                .disabled(op_running || ahead == 0)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.side_panel.git_push();
                                    cx.notify();
                                })),
                        )
                        .when(op_running, |row| row.child(Spinner::new().xsmall()))
                        .into_any_element(),
                ),
                Some(VcsKind::Svn) => Some(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(
                            Button::new("svn-add-all")
                                .label("添加")
                                .small()
                                .disabled(op_running || !svn_add_ready)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.side_panel.svn_add_all();
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("svn-update")
                                .label("更新")
                                .small()
                                .disabled(op_running)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.side_panel.git_pull();
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("svn-log")
                                .label("日志")
                                .small()
                                .disabled(op_running)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.side_panel.svn_log();
                                    cx.notify();
                                })),
                        )
                        .child(
                            // 面板自己的列表只看本地；"别人改了什么"要靠这个连远端比。
                            Button::new("svn-check-modifications")
                                .label("检查修改")
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.side_panel.svn_check_modifications();
                                    cx.notify();
                                })),
                        )
                        .child(Self::svn_more_menu(&menu_target))
                        .when(op_running, |row| row.child(Spinner::new().xsmall()))
                        .into_any_element(),
                ),
                Some(VcsKind::SvnRepository) => Some(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(Button::new("svn-browse-repository").label("浏览").small().on_click(
                            cx.listener(|this, _, _, cx| {
                                this.side_panel.svn_browse_repository();
                                cx.notify();
                            }),
                        ))
                        .child(
                            Button::new("svn-checkout-repository").label("检出").small().on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.side_panel.svn_checkout_repository();
                                    cx.notify();
                                }),
                            ),
                        )
                        .child(Button::new("svn-repository-log").label("日志").small().on_click(
                            cx.listener(|this, _, _, cx| {
                                this.side_panel.svn_repository_log();
                                cx.notify();
                            }),
                        ))
                        .child(
                            // 三个目录齐了就没什么可建的，禁掉比让用户点出一个
                            // "目录已存在"的错误好。
                            Button::new("svn-standard-layout")
                                .label("建标准布局")
                                .small()
                                .disabled(op_running || standard_layout_done)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.side_panel.svn_create_standard_layout();
                                    cx.notify();
                                })),
                        )
                        .child(Button::new("svn-repository-import").label("导入").small().on_click(
                            cx.listener(|this, _, _, cx| {
                                this.side_panel.svn_repository_import();
                                cx.notify();
                            }),
                        ))
                        .when(op_running, |row| row.child(Spinner::new().xsmall()))
                        .into_any_element(),
                ),
                None => None,
            }
        };
        let vcs_label =
            if matches!(vcs, Some(VcsKind::Svn | VcsKind::SvnRepository)) { "SVN" } else { "Git" };
        // 只在 SVN 下问一次（`tortoise_available` 内部缓存，之后免费）。这条提示
        // 存在的理由：没装 TortoiseSVN 时，日志/锁定/属性这些按钮点下去只会在
        // 错误栏里蹦一句话——把"为什么不能用"提前讲清楚，比让用户逐个试出来好。
        let tortoise_missing = matches!(vcs, Some(VcsKind::Svn | VcsKind::SvnRepository))
            && !crate::display::side_panel::tortoise_available();

        v_flex()
            .h_full()
            .w(px(320.0))
            .flex_shrink_0()
            .p_2()
            .gap_2()
            // 与文件树共用抽屉接缝合同（见 `render_file_tree`）：圆角只给左侧两角，
            // 右缘贴窗口边框不倒角，四边无描边。
            .rounded_tl(crate::gpui_shell::theme::card_radius(cx))
            .rounded_bl(crate::gpui_shell::theme::card_radius(cx))
            .bg(theme.popover)
            // 与文件树抽屉同一套紧凑投影，避免右侧抽屉语言再出现 shadow_lg。
            .shadow(gpui_component::popover_shadow(theme.is_dark()))
            .occlude()
            .child(view_switch)
            // VCS 状态现在跟着侧栏定位走（`SidePanel::vcs_root`），所以必须在
            // **这个视图里**给出回头路：在树里点 `..` 翻出仓库、或"打开目录"
            // 选到别处之后，用户得能一键回到终端当前目录。此前这个入口只画在
            // 文件视图里，那正是当年不敢让 VCS 跟随定位的原因。
            .when(browsing_elsewhere, |panel| {
                panel.child(
                    Button::new("vcs-follow-cwd")
                        .label("回到终端当前目录")
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.sync_side_panel_to_active(true, cx);
                            cx.notify();
                        })),
                )
            })
            .when_some(summary, |panel, summary| panel.child(summary))
            .when_some(git_nav, |panel, nav| panel.child(nav))
            .when_some(history_head, |panel, head| panel.child(head))
            .when_some(repository_notice, |panel, notice| panel.child(notice))
            .when(tortoise_missing, |panel| {
                panel.child(div().text_xs().text_color(theme.warning).child(
                    "未检测到 TortoiseSVN：日志、锁定、属性等对话框不可用（提交与更新仍可走 svn 命令行）",
                ))
            })
            .when_some(op_error, |panel, error| {
                panel.child(div().text_xs().text_color(theme.danger).child(error))
            })
            .when_some(commit_row, |panel, row| panel.child(row))
            .when_some(action_strip, |panel, row| panel.child(row))
            .when(git.is_none(), |panel| {
                panel.child(
                    div().py_3().text_sm().text_color(muted).child("当前目录不在 Git/SVN 仓库中"),
                )
            })
            .when_some(self.side_panel.root_notice(), |panel, notice| {
                panel.child(div().text_xs().text_color(theme.warning).child(notice.to_owned()))
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(v_flex().w_full().gap_1().children(rows)),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_1()
                    .child(
                        Button::new("git-tree-refresh")
                            .icon(IconName::Redo2)
                            .ghost()
                            .xsmall()
                            .tooltip(format!("刷新 {vcs_label} 状态"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.side_panel.request_refresh();
                                this.sync_side_panel_to_active(false, cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("git-tree-close")
                            .icon(IconName::Close)
                            .ghost()
                            .xsmall()
                            .tooltip(format!("关闭 {vcs_label} 状态"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_git_tree(cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    /// SVN 工作副本的"更多操作"下拉。
    ///
    /// 为什么是菜单而不是一排按钮：小乌龟右键菜单里的常用项有十几个，全铺成
    /// 按钮会把 320px 宽的抽屉挤爆，而它们的使用频率差着一个数量级——更新、
    /// 提交、日志是每天几十次，重定位和修订图是几个月一次。常驻位留给前者，
    /// 后者收进这里，位置固定、找得到就够。
    fn svn_more_menu(target: &gpui::WeakEntity<Self>) -> impl IntoElement {
        let target = target.clone();
        Button::new("svn-more")
            .icon(IconName::Ellipsis)
            .small()
            .tooltip("更多 SVN 操作")
            .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
                menu.external_link_icon(false)
                    .item(svn_menu_item("版本库浏览器", &target, |panel| {
                        panel.svn_browse_working_copy();
                    }))
                    .item(svn_menu_item("更新至版本…", &target, |panel| {
                        panel.svn_update_to_revision();
                    }))
                    .separator()
                    .item(svn_menu_item("分支 / 标记…", &target, |panel| {
                        panel.svn_branch_or_tag();
                    }))
                    .item(svn_menu_item("切换…", &target, |panel| {
                        panel.svn_switch();
                    }))
                    .item(svn_menu_item("合并…", &target, |panel| {
                        panel.svn_merge();
                    }))
                    .separator()
                    .item(svn_menu_item("导出…", &target, |panel| {
                        panel.svn_export();
                    }))
                    .item(svn_menu_item("修订图", &target, |panel| {
                        panel.svn_revision_graph();
                    }))
                    .item(svn_menu_item("重定位…", &target, |panel| {
                        panel.svn_relocate();
                    }))
                    .separator()
                    .item(svn_menu_item("属性", &target, |panel| {
                        panel.svn_properties();
                    }))
                    .item(svn_menu_item("清理", &target, |panel| {
                        panel.svn_cleanup();
                    }))
            })
    }

    /// 某一行文件的 SVN 操作下拉。
    ///
    /// 锁定/解锁在这里而不在行内图标位：行宽有限，而这两个动作虽然是 SVN 的
    /// 招牌能力，单次点击频率仍低于"看差异"。菜单里的次序按真实使用频率排。
    fn svn_row_menu(
        target: &gpui::WeakEntity<Self>,
        path: &str,
        index: usize,
        section: &str,
    ) -> impl IntoElement {
        let target = target.clone();
        let path = path.to_owned();
        Button::new(SharedString::from(format!("svn-row-more-{section}-{index}")))
            .icon(IconName::Ellipsis)
            .ghost()
            .xsmall()
            .tooltip("此文件的 SVN 操作")
            .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
                let item = |label: &'static str,
                            action: fn(&mut crate::display::side_panel::SidePanel, &str)| {
                    svn_row_menu_item(label, &target, &path, action)
                };
                menu.external_link_icon(false)
                    .item(item("显示日志", |panel, path| {
                        panel.svn_log_path(path);
                    }))
                    .item(item("责任追溯", |panel, path| {
                        panel.svn_blame_path(path);
                    }))
                    .separator()
                    .item(item("获得锁定…", |panel, path| {
                        panel.svn_lock_path(path);
                    }))
                    .item(item("释放锁定", |panel, path| {
                        panel.svn_unlock_path(path);
                    }))
                    .separator()
                    .item(item("加入忽略列表…", |panel, path| {
                        panel.svn_ignore_path(path);
                    }))
                    .item(item("重命名…", |panel, path| {
                        panel.svn_rename_path(path);
                    }))
                    .item(item("删除", |panel, path| {
                        panel.svn_delete_path(path);
                    }))
                    .separator()
                    .item(item("编辑冲突", |panel, path| {
                        panel.svn_conflict_editor_path(path);
                    }))
                    .item(item("属性", |panel, path| {
                        panel.svn_properties_path(path);
                    }))
            })
    }
}

/// 一条活动线路指向后续仍会显示的提交。`id` 区分指向同一祖先的两条线路：
/// 它们必须保持各自颜色，直到在那个祖先节点上真正汇合。
#[derive(Debug, Clone, PartialEq, Eq)]
struct GitActiveLane {
    id: u64,
    target: String,
    color: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitLaneEdgeKind {
    /// 不经过本行提交的活动线路，从行顶连续画到行底。
    Through,
    /// 一条线路在当前提交结束，从行顶汇入节点。
    IntoNode,
    /// 当前提交通向一个父提交，从节点分出并延伸到行底。
    OutOfNode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GitLaneEdge {
    from_lane: usize,
    to_lane: usize,
    color: usize,
    kind: GitLaneEdgeKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct GitGraphRow {
    node_lane: usize,
    node_color: usize,
    merge_commit: bool,
    lane_count: usize,
    edges: Vec<GitLaneEdge>,
}

fn allocate_git_lane_color(active: &[GitActiveLane], next_color: &mut usize) -> usize {
    const COLOR_COUNT: usize = 4;
    for offset in 0..COLOR_COUNT {
        let candidate = (*next_color + offset) % COLOR_COUNT;
        if !active.iter().any(|lane| lane.color == candidate) {
            *next_color = (candidate + 1) % COLOR_COUNT;
            return candidate;
        }
    }
    let color = *next_color % COLOR_COUNT;
    *next_color = (*next_color + 1) % COLOR_COUNT;
    color
}

/// 直接依据提交对象和父提交关系生成线路。每个提交严格对应一个视觉行；分支
/// 颜色跟随线路身份，即使 lane 横向移动也不会换色。指向同一祖先的线路保留
/// 到祖先行再汇合，因此汇合位置与 Git DAG 的语义一致。
fn git_graph_rows(commits: &[crate::display::side_panel::GitCommit]) -> Vec<GitGraphRow> {
    let mut active = Vec::<GitActiveLane>::new();
    let mut next_lane_id = 0_u64;
    let mut next_color = 0_usize;
    let mut rows = Vec::with_capacity(commits.len());

    for commit in commits {
        let matching = active
            .iter()
            .enumerate()
            .filter_map(|(index, lane)| (lane.target == commit.full_hash).then_some(index))
            .collect::<Vec<_>>();
        let node_lane = matching.first().copied().unwrap_or(active.len());
        let node_color = matching.first().map_or_else(
            || allocate_git_lane_color(&active, &mut next_color),
            |index| active[*index].color,
        );

        let mut bottom = Vec::with_capacity(
            active.len().saturating_sub(matching.len()) + commit.parent_hashes.len(),
        );
        let mut insert_at = 0;
        for (index, lane) in active.iter().enumerate() {
            if lane.target == commit.full_hash {
                continue;
            }
            if index < node_lane {
                insert_at += 1;
            }
            bottom.push(lane.clone());
        }

        let mut parent_lanes = Vec::with_capacity(commit.parent_hashes.len());
        for (parent_index, parent) in commit.parent_hashes.iter().enumerate() {
            let color = if parent_index == 0 {
                node_color
            } else {
                let mut occupied = bottom.clone();
                occupied.extend(parent_lanes.iter().cloned());
                allocate_git_lane_color(&occupied, &mut next_color)
            };
            parent_lanes.push(GitActiveLane { id: next_lane_id, target: parent.clone(), color });
            next_lane_id = next_lane_id.wrapping_add(1);
        }
        let parent_count = parent_lanes.len();
        bottom.splice(insert_at..insert_at, parent_lanes);

        let mut edges = Vec::new();
        for (top_lane, lane) in active.iter().enumerate() {
            if lane.target == commit.full_hash {
                continue;
            }
            if let Some(bottom_lane) = bottom.iter().position(|candidate| candidate.id == lane.id) {
                edges.push(GitLaneEdge {
                    from_lane: top_lane,
                    to_lane: bottom_lane,
                    color: lane.color,
                    kind: GitLaneEdgeKind::Through,
                });
            }
        }
        for top_lane in matching {
            edges.push(GitLaneEdge {
                from_lane: top_lane,
                to_lane: node_lane,
                color: active[top_lane].color,
                kind: GitLaneEdgeKind::IntoNode,
            });
        }
        for parent_offset in 0..parent_count {
            let bottom_lane = insert_at + parent_offset;
            edges.push(GitLaneEdge {
                from_lane: node_lane,
                to_lane: bottom_lane,
                color: bottom[bottom_lane].color,
                kind: GitLaneEdgeKind::OutOfNode,
            });
        }

        rows.push(GitGraphRow {
            node_lane,
            node_color,
            merge_commit: commit.parent_hashes.len() > 1,
            lane_count: active.len().max(bottom.len()).max(node_lane + 1),
            edges,
        });
        active = bottom;
    }
    rows
}

fn git_lane_layout(rows: &[GitGraphRow]) -> (f32, f32) {
    let lanes = rows.iter().map(|row| row.lane_count).max().unwrap_or(1).max(1);
    let width = (18.0 + lanes.saturating_sub(1) as f32 * 13.0).clamp(36.0, 112.0);
    let spacing = if lanes <= 1 { 13.0 } else { ((width - 20.0) / (lanes - 1) as f32).min(13.0) };
    (width, spacing)
}

fn paint_git_lane_line(window: &mut Window, color: gpui::Hsla, from: (f32, f32), to: (f32, f32)) {
    let mut path = gpui::PathBuilder::stroke(px(2.0));
    path.move_to(gpui::point(px(from.0), px(from.1)));
    path.line_to(gpui::point(px(to.0), px(to.1)));
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn paint_git_lane_connection(
    window: &mut Window,
    color: gpui::Hsla,
    from: (f32, f32),
    to: (f32, f32),
) {
    if (from.0 - to.0).abs() < 0.5 {
        paint_git_lane_line(window, color, from, to);
        return;
    }
    let middle = ((from.0 + to.0) * 0.5, (from.1 + to.1) * 0.5);
    let mut path = gpui::PathBuilder::stroke(px(2.0));
    path.move_to(gpui::point(px(from.0), px(from.1)));
    path.curve_to(gpui::point(px(middle.0), px(middle.1)), gpui::point(px(from.0), px(middle.1)));
    path.curve_to(gpui::point(px(to.0), px(to.1)), gpui::point(px(to.0), px(middle.1)));
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

#[allow(clippy::too_many_arguments)]
fn git_lane_canvas(
    graph: GitGraphRow,
    width: f32,
    lane_spacing: f32,
    colors: [gpui::Hsla; 4],
    surface: gpui::Hsla,
) -> gpui::AnyElement {
    div()
        .relative()
        .w(px(width))
        .h_full()
        .flex_shrink_0()
        .overflow_hidden()
        .child(
            gpui::canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    let origin_x = f32::from(bounds.origin.x);
                    let origin_y = f32::from(bounds.origin.y);
                    let height = f32::from(bounds.size.height);
                    let right = origin_x + f32::from(bounds.size.width);
                    let top = origin_y;
                    let middle_y = origin_y + height * 0.5;
                    let bottom = origin_y + height;
                    let lane_x = |lane: usize| origin_x + 10.0 + lane as f32 * lane_spacing;
                    let lane_color = |lane: usize| colors[lane % colors.len()];

                    // 先画线路、最后画节点，让多条曲线的交汇点保持干净。
                    for edge in &graph.edges {
                        let from_x = lane_x(edge.from_lane);
                        let to_x = lane_x(edge.to_lane);
                        if from_x >= right || to_x >= right {
                            continue;
                        }
                        let (from_y, to_y) = match edge.kind {
                            GitLaneEdgeKind::Through => (top, bottom),
                            GitLaneEdgeKind::IntoNode => (top, middle_y),
                            GitLaneEdgeKind::OutOfNode => (middle_y, bottom),
                        };
                        paint_git_lane_connection(
                            window,
                            lane_color(edge.color),
                            (from_x, from_y),
                            (to_x, to_y),
                        );
                    }

                    let x = lane_x(graph.node_lane);
                    if x < right {
                        let color = lane_color(graph.node_color);
                        let radius = if graph.merge_commit { 5.0 } else { 4.0 };
                        window.paint_quad(
                            gpui::fill(
                                gpui::Bounds::new(
                                    gpui::point(px(x - radius), px(middle_y - radius)),
                                    gpui::size(px(radius * 2.0), px(radius * 2.0)),
                                ),
                                color,
                            )
                            .corner_radii(px(radius)),
                        );
                        if graph.merge_commit {
                            let inner = 2.5;
                            window.paint_quad(
                                gpui::fill(
                                    gpui::Bounds::new(
                                        gpui::point(px(x - inner), px(middle_y - inner)),
                                        gpui::size(px(inner * 2.0), px(inner * 2.0)),
                                    ),
                                    surface,
                                )
                                .corner_radii(px(inner)),
                            );
                        }
                    }
                },
            )
            .absolute()
            .inset_0(),
        )
        .into_any_element()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitRefKind {
    Head,
    Local,
    Remote,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitRefLabel {
    label: String,
    kind: GitRefKind,
}

fn git_ref_labels(decorations: &str) -> Vec<GitRefLabel> {
    let mut labels = Vec::new();
    for raw in decorations.split(',').map(str::trim).filter(|value| !value.is_empty()) {
        let (label, kind) = if let Some(branch) = raw.strip_prefix("HEAD -> refs/heads/") {
            (branch, GitRefKind::Head)
        } else if let Some(branch) = raw.strip_prefix("HEAD -> ") {
            (branch, GitRefKind::Head)
        } else if raw == "HEAD" {
            (raw, GitRefKind::Head)
        } else if let Some(tag) = raw.strip_prefix("tag: refs/tags/") {
            (tag, GitRefKind::Tag)
        } else if let Some(tag) = raw.strip_prefix("tag: ") {
            (tag, GitRefKind::Tag)
        } else if let Some(remote) = raw.strip_prefix("refs/remotes/") {
            (remote, GitRefKind::Remote)
        } else if let Some(branch) = raw.strip_prefix("refs/heads/") {
            (branch, GitRefKind::Local)
        } else {
            // `--decorate=full` 给常规分支带明确前缀；未知命名空间不能仅因
            // 含 `/` 就冒充远端分支（本地分支名本来就允许 feature/foo）。
            (raw, GitRefKind::Local)
        };
        if !labels.iter().any(|entry: &GitRefLabel| entry.label == label) {
            labels.push(GitRefLabel { label: label.to_owned(), kind });
        }
    }
    labels
}

/// 一个"点了就在 side_panel 上跑一个无参 SVN 操作"的菜单项。用 `fn` 指针而
/// 不是闭包：这些操作都不捕获环境，指针让每个菜单项塌成一行。
fn svn_menu_item(
    label: &'static str,
    workspace: &gpui::WeakEntity<NebulaWorkspace>,
    action: fn(&mut crate::display::side_panel::SidePanel),
) -> PopupMenuItem {
    let workspace = workspace.clone();
    PopupMenuItem::new(label).on_click(move |_, _, cx| {
        if let Some(workspace) = workspace.upgrade() {
            workspace.update(cx, |this, cx| {
                action(&mut this.side_panel);
                cx.notify();
            });
        }
    })
}

/// 同上，但操作作用在某一行的相对路径上。
fn svn_row_menu_item(
    label: &'static str,
    workspace: &gpui::WeakEntity<NebulaWorkspace>,
    path: &str,
    action: fn(&mut crate::display::side_panel::SidePanel, &str),
) -> PopupMenuItem {
    let workspace = workspace.clone();
    let path = path.to_owned();
    PopupMenuItem::new(label).on_click(move |_, _, cx| {
        if let Some(workspace) = workspace.upgrade() {
            workspace.update(cx, |this, cx| {
                action(&mut this.side_panel, &path);
                cx.notify();
            });
        }
    })
}

/// 版本库体积的人读形式。摘要里这一项是给"这库多大、该不该整个检出"用的，
/// 一位小数足够，不做二进制/十进制单位之争。
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

/// 提交时间的人读形式。Git 提供 Unix 秒，显示层自己计算，结果不受机器上
/// `git log` 的 locale 影响；未来时间（系统时钟回拨）按“刚刚”处理。
fn git_relative_time(timestamp: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(timestamp);
    git_relative_time_at(timestamp, now)
}

fn git_relative_time_at(timestamp: i64, now: i64) -> String {
    let seconds = now.saturating_sub(timestamp).max(0);
    match seconds {
        0..=59 => "刚刚".to_owned(),
        60..=3599 => format!("{} 分钟前", seconds / 60),
        3600..=86_399 => format!("{} 小时前", seconds / 3600),
        86_400..=2_592_000 => format!("{} 天前", seconds / 86_400),
        2_592_001..=31_536_000 => format!("{} 个月前", seconds / 2_592_000),
        _ => format!("{} 年前", seconds / 31_536_000),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GitLaneEdgeKind, GitRefKind, git_graph_rows, git_ref_labels, git_relative_time_at,
    };
    use crate::display::side_panel::GitCommit;

    #[test]
    fn git_graph_preserves_branch_identity_until_the_real_ancestor() {
        let commit = |hash: &str, parents: &[&str]| GitCommit {
            full_hash: hash.to_owned(),
            short_hash: hash.to_owned(),
            parent_hashes: parents.iter().map(|parent| (*parent).to_owned()).collect(),
            ..GitCommit::default()
        };
        let rows = git_graph_rows(&[
            commit("merge", &["main", "topic"]),
            commit("main", &["base"]),
            commit("topic", &["base"]),
            commit("base", &[]),
        ]);

        assert_eq!(rows.len(), 4, "one visual row per commit, without topology spacer rows");
        assert!(rows[0].merge_commit);
        assert!(rows[0].edges.iter().any(|edge| {
            edge.kind == GitLaneEdgeKind::OutOfNode
                && edge.from_lane == 0
                && edge.to_lane == 1
                && edge.color == 1
        }));
        assert!(rows[3].edges.iter().any(|edge| {
            edge.kind == GitLaneEdgeKind::IntoNode
                && edge.from_lane == 1
                && edge.to_lane == 0
                && edge.color == 1
        }));
    }

    #[test]
    fn git_refs_keep_head_remote_and_tag_meanings() {
        let refs = git_ref_labels(concat!(
            "HEAD -> refs/heads/main, tag: refs/tags/v1.5.0, ",
            "refs/remotes/origin/main, refs/heads/feature/ui"
        ));
        assert_eq!(refs.len(), 4);
        assert_eq!(refs[0].label, "main");
        assert_eq!(refs[0].kind, GitRefKind::Head);
        assert_eq!(refs[1].label, "v1.5.0");
        assert_eq!(refs[1].kind, GitRefKind::Tag);
        assert_eq!(refs[2].kind, GitRefKind::Remote);
        assert_eq!(refs[3].label, "feature/ui");
        assert_eq!(refs[3].kind, GitRefKind::Local);
    }

    #[test]
    fn git_history_relative_time_uses_stable_boundaries() {
        let now = 100_000_000;
        assert_eq!(git_relative_time_at(now + 1, now), "刚刚");
        assert_eq!(git_relative_time_at(now - 59, now), "刚刚");
        assert_eq!(git_relative_time_at(now - 60, now), "1 分钟前");
        assert_eq!(git_relative_time_at(now - 3_600, now), "1 小时前");
        assert_eq!(git_relative_time_at(now - 86_400, now), "1 天前");
        assert_eq!(git_relative_time_at(now - 2_592_001, now), "1 个月前");
        assert_eq!(git_relative_time_at(now - 31_536_001, now), "1 年前");
    }
}
