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
        let symbol: SharedString = crate::font_install::REQUIRED_FONT_FAMILY.into();
        // VCS 状态跟着 `SidePanel::vcs_root`：显式浏览定位优先，其次终端 cwd。
        let root = self.side_panel.vcs_root().map(Path::to_path_buf);
        let selected = self.side_panel.selected.clone();
        let git = self.side_panel.git().cloned();
        let vcs = git.as_ref().map(|info| info.vcs);
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
            use crate::display::side_panel::VcsKind;
            /// 分组决定行内操作（VS Code 的 SCM 行合同）。
            #[derive(Clone, Copy, PartialEq)]
            enum RowOps {
                /// 变更组：暂存 + 丢弃（untracked 不给丢弃——restore 不删新文件）。
                Unstaged,
                /// 已暂存组：取消暂存。
                Staged,
                /// Git 冲突组：当前只展示状态。
                None,
                /// SVN 无暂存区，具体操作由状态字母决定。
                Svn,
            }
            let is_git = info.vcs == VcsKind::Git;
            let conflict_paths: std::collections::HashSet<&str> =
                info.conflicts.iter().map(|(_, path)| path.as_str()).collect();
            // VS Code 三组模型：合并冲突 → 已暂存 → 变更。冲突路径从后两组
            // 过滤（数据层为旧壳兼容把它们同时留在原列表里）。
            let mut sections: Vec<(&str, Vec<&(char, String)>, RowOps)> = Vec::new();
            if !info.conflicts.is_empty() {
                sections.push((
                    "合并冲突",
                    info.conflicts.iter().collect(),
                    if is_git { RowOps::None } else { RowOps::Svn },
                ));
            }
            let not_conflicted =
                |(_, path): &&(char, String)| !conflict_paths.contains(path.as_str());
            match info.vcs {
                VcsKind::Git => {
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
                        .child("没有更改")
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
                        Some(pos) => {
                            (relative_path[pos + 1..].to_owned(), relative_path[..pos].to_owned())
                        },
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
                    let discard_armed = discard_confirm.as_deref() == Some(relative_path.as_str());
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
                                    .child(div().flex_shrink_0().text_sm().child(file_name.clone()))
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
                                            button.icon(IconName::Undo2).ghost().xsmall().tooltip(
                                                if svn_revert {
                                                    "还原 SVN 改动"
                                                } else {
                                                    "丢弃改动"
                                                },
                                            )
                                        }
                                    })
                                    .when(!discard_armed, |button| {
                                        button
                                            .invisible()
                                            .group_hover(row_group.clone(), |button| {
                                                button.visible()
                                            })
                                    })
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            if this.vcs_discard_confirm.as_deref()
                                                == Some(discard_path.as_str())
                                            {
                                                this.vcs_discard_confirm = None;
                                                if svn_revert {
                                                    this.side_panel.svn_revert_path(&discard_path);
                                                } else {
                                                    this.side_panel.git_discard_path(&discard_path);
                                                }
                                            } else {
                                                this.vcs_discard_confirm =
                                                    Some(discard_path.clone());
                                            }
                                            cx.notify();
                                        },
                                    )),
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
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.vcs_discard_confirm = None;
                                            this.side_panel.git_stage_path(&stage_path);
                                            cx.notify();
                                        },
                                    )),
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
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.vcs_discard_confirm = None;
                                            this.side_panel.svn_add_path(&svn_add_path);
                                            cx.notify();
                                        },
                                    )),
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
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.vcs_discard_confirm = None;
                                            this.side_panel.svn_resolve_path(&resolve_path);
                                            cx.notify();
                                        },
                                    )),
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
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.vcs_discard_confirm = None;
                                            this.side_panel.git_unstage_path(&unstage_path);
                                            cx.notify();
                                        },
                                    )),
                                )
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.side_panel.selected = Some(path.clone());
                                cx.notify();
                            }))
                            .on_double_click(cx.listener(move |this, _, window, cx| {
                                if !svn_diff || !this.side_panel.svn_diff_path(&diff_path) {
                                    // Git 与未版本化 SVN 文件仍走现有文档路由。
                                    this.open_document_path(open_path.clone(), window, cx);
                                }
                            }))
                            .into_any_element(),
                    );
                }
            }
        }

        use crate::display::side_panel::VcsKind;
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
        let commit_row = git.as_ref().filter(|info| info.vcs != VcsKind::SvnRepository).map(|_| {
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
        let action_strip = match vcs {
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
                        Button::new("svn-log").label("日志").small().disabled(op_running).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.side_panel.svn_log();
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        // 面板自己的列表只看本地；"别人改了什么"要靠这个连远端比。
                        Button::new("svn-check-modifications").label("检查修改").small().on_click(
                            cx.listener(|this, _, _, cx| {
                                this.side_panel.svn_check_modifications();
                                cx.notify();
                            }),
                        ),
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
                    .child(Button::new("svn-checkout-repository").label("检出").small().on_click(
                        cx.listener(|this, _, _, cx| {
                            this.side_panel.svn_checkout_repository();
                            cx.notify();
                        }),
                    ))
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
