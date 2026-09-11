//! Cached host search, grouping, virtual rows and credential-free exchange.

use super::*;
use gpui::AppContext as _;
use gpui_component::menu::PopupMenuItem;

mod exchange;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::gpui_shell) enum HostScope {
    All,
    Managed,
    Recent,
}

pub(in crate::gpui_shell) struct HostLibraryState {
    pub search: Entity<InputState>,
    pub group: Entity<InputState>,
    pub tags: Entity<InputState>,
    pub notes: Entity<InputState>,
    scope: HostScope,
    group_filter: Option<String>,
    scroll: gpui::UniformListScrollHandle,
    pub(super) busy: bool,
    sequence: u64,
}

impl HostLibraryState {
    pub(in crate::gpui_shell) fn new(window: &mut Window, cx: &mut Context<SettingsPane>) -> Self {
        let language = crate::gpui_shell::config::ui_language(cx);
        Self {
            search: cx.new(|cx| {
                InputState::new(window, cx).placeholder(language.text(Message::HostsSearch))
            }),
            group: cx.new(|cx| {
                InputState::new(window, cx).placeholder(language.text(Message::HostsGroup))
            }),
            tags: cx.new(|cx| {
                InputState::new(window, cx).placeholder(language.text(Message::HostsTagsHint))
            }),
            notes: cx.new(|cx| InputState::new(window, cx).multi_line(true).soft_wrap(true)),
            scope: HostScope::All,
            group_filter: None,
            scroll: Default::default(),
            busy: false,
            sequence: 0,
        }
    }

    pub(in crate::gpui_shell) fn reset_scroll(&self) {
        self.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
    }
}

impl SettingsPane {
    fn filtered_library_hosts(&self, cx: &gpui::App) -> Vec<String> {
        let query = self.ssh_library.search.read(cx).value();
        let hosts = if self.ssh_library.scope == HostScope::Recent {
            self.ssh_hosts
                .saved
                .iter()
                .filter(|host| !self.ssh_hosts.hidden.contains(host))
                .cloned()
                .collect()
        } else {
            self.ssh_hosts.merged()
        };
        self.ssh_hosts.profiles.filter_hosts(
            hosts,
            &query,
            self.ssh_library.scope == HostScope::Managed,
            self.ssh_library.group_filter.as_deref(),
        )
    }

    fn library_controls(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let language = crate::gpui_shell::config::ui_language(cx);
        let weak = cx.entity().downgrade();
        let group_filter = self.ssh_library.group_filter.clone();
        let groups = self.ssh_hosts.profiles.groups();
        let group_label = match group_filter.as_deref() {
            None => language.text(Message::HostsAllGroups).to_owned(),
            Some("") => language.text(Message::HostsUngrouped).to_owned(),
            Some(group) => group.to_owned(),
        };
        v_flex()
            .gap_2()
            .mb_3()
            .child(Input::new(&self.ssh_library.search).w_full())
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .children(
                        [
                            (HostScope::All, Message::HostsAll),
                            (HostScope::Managed, Message::HostsManaged),
                            (HostScope::Recent, Message::HostsRecent),
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(index, (scope, label))| {
                            Button::new(("host-scope", index))
                                .label(language.text(label))
                                .small()
                                .selected(self.ssh_library.scope == scope)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.ssh_library.scope = scope;
                                    this.ssh_library.reset_scroll();
                                    cx.notify();
                                }))
                        }),
                    )
                    .child(
                        Button::new("host-group-filter").label(group_label).small().dropdown_menu(
                            move |mut menu, _, _| {
                                let choices = std::iter::once((
                                    None,
                                    language.text(Message::HostsAllGroups).to_owned(),
                                ))
                                .chain(std::iter::once((
                                    Some(String::new()),
                                    language.text(Message::HostsUngrouped).to_owned(),
                                )))
                                .chain(
                                    groups.iter().map(|group| (Some(group.clone()), group.clone())),
                                );
                                for (choice, label) in choices {
                                    let owner = weak.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(label)
                                            .checked(choice == group_filter)
                                            .on_click(move |_, _, cx| {
                                                let _ = owner.update(cx, |this, cx| {
                                                    this.ssh_library.group_filter = choice.clone();
                                                    this.ssh_library.reset_scroll();
                                                    cx.notify();
                                                });
                                            }),
                                    );
                                }
                                menu
                            },
                        ),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("hosts-import-csv")
                            .label(language.text(Message::HostsImport))
                            .small()
                            .disabled(self.ssh_library.busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.import_host_csv(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("hosts-export-csv")
                            .label(language.text(Message::HostsExport))
                            .small()
                            .disabled(self.ssh_library.busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.export_host_csv(window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn host_organization_fields(
        &self,
        window: &Window,
        cx: &gpui::App,
    ) -> gpui::AnyElement {
        let language = crate::gpui_shell::config::ui_language(cx);
        v_flex()
            .mt_4()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_xs().child(language.text(Message::HostsGroup)))
                            .child(editor::editor_input(
                                &self.ssh_library.group,
                                language.text(Message::HostsGroup),
                                window,
                                cx,
                            )),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_xs().child(language.text(Message::HostsTags)))
                            .child(editor::editor_input(
                                &self.ssh_library.tags,
                                language.text(Message::HostsTags),
                                window,
                                cx,
                            )),
                    ),
            )
            .child(div().text_xs().child(language.text(Message::HostsNotes)))
            .child(Input::new(&self.ssh_library.notes).w_full().h(px(80.0)))
            .into_any_element()
    }

    fn render_library_host(
        &self,
        host: String,
        ix: usize,
        host_count: usize,
        labels: &std::collections::HashMap<String, String>,
        icons: &std::collections::HashMap<String, String>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let language = crate::gpui_shell::config::ui_language(cx);
        let theme = cx.theme();
        let hover_bg = crate::gpui_shell::theme::settings_hover_bg(cx, false);
        let muted = theme.muted_foreground;
        let symbol_family: SharedString = crate::font_install::REQUIRED_FONT_FAMILY.into();
        let font_px = cx
            .try_global::<crate::gpui_shell::config::Settings>()
            .map(|s| s.base_font_size_px)
            .unwrap_or(15.0);
        let title_h = font_px;
        let subtitle_h = font_px * 0.78;
        let delete_confirm = self.ssh_delete_confirm.clone();

        let pinned = self.ssh_hosts.is_pinned(&host);
        let from_config = self.ssh_hosts.is_from_config(&host);
        let confirm = delete_confirm.as_deref() == Some(host.as_str());
        let label = labels.get(&host).cloned().unwrap_or_else(|| host.clone());
        // 行首 OS 图标（旧壳裁定 2026-08-09）：id 取自 ssh_profiles 存储，
        // 未认出回落通用终端形状；mono 字体渲染 Nerd Font 字位。
        let os_icon = crate::display::ui::os_icons::resolve(icons.get(&host).map(String::as_str));
        let connect_host = host.clone();
        let edit_host = host.clone();
        let pin_host = host.clone();
        let delete_host = host.clone();
        let row_group = SharedString::from(format!("ssh-host-actions-{ix}"));
        h_flex()
                .id(SharedString::from(format!("ssh-host-row-{ix}")))
                .group(row_group.clone())
                // 旧壳 `SSH_HOST_ROW_H` 固定 58px；两行文字与 OS 图标在
                // 这个高度里共用中线，不能压成普通 48px 设置行。
                .h(px(SSH_HOST_ROW_H))
                .w_full()
                .px_3()
                .items_center()
                .gap_3()
                .when(ix == 0, |row| row.rounded_t(px(8.0)))
                .when(ix + 1 == host_count, |row| row.rounded_b(px(8.0)))
                .when(ix + 1 < host_count, |row| {
                    row.border_b_1().border_color(theme.border.opacity(0.5))
                })
                .hover(move |row| row.bg(hover_bg))
                .child(
                    div()
                        .w(px(22.0))
                        .h_full()
                        .flex_shrink_0()
                        .relative()
                        .flex()
                        .items_center()
                        .justify_center()
                        .font_family(symbol_family.clone())
                        .text_size(px(18.0))
                        .text_color(muted)
                        .text_center()
                        .child(os_icon.glyph.to_string()),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .justify_center()
                        .child(
                            div()
                                .h(px(title_h * 0.95))
                                .flex()
                                .items_center()
                                .text_size(px(title_h))
                                .line_height(px(title_h))
                                .truncate()
                                .gap_2()
                                .child(label)
                                .child(div().text_xs().text_color(muted)
                                    .child(self.ssh_hosts.profiles.organization(&host).group.clone())),
                        )
                        .child(
                            h_flex()
                                .h(px(subtitle_h))
                                .gap_2()
                                .items_center()
                                // 副行与旧壳同合同：只放目的地本身；来源用
                                // 小徽章表达（config 源的删除语义是隐藏）。
                                .child(
                                    div()
                                        .text_size(px(subtitle_h))
                                        .line_height(px(subtitle_h))
                                        .text_color(muted)
                                        .truncate()
                                        .child(host.clone()),
                                )
                                .when(from_config, |line| {
                                    line.child(
                                        div()
                                            .flex_shrink_0()
                                            .px(px(5.0))
                                            .rounded_sm()
                                            .text_xs()
                                            .text_color(muted)
                                            .border_1()
                                            .border_color(theme.border)
                                            .child("config"),
                                    )
                                }),
                        ),
                )
                .child(
                    Button::new(SharedString::from(format!("ssh-connect-{ix}")))
                        .label(language.pick("连接", "Connect"))
                        .small()
                        .primary()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.emit(SettingsPaneEvent::LaunchSsh(connect_host.clone()));
                            this.ssh_status = Some(SshStatus::Opening(connect_host.clone()));
                            cx.notify();
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("ssh-edit-{ix}")))
                        .icon(IconName::Settings2)
                        .ghost()
                        .small()
                        .tooltip(language.pick("编辑主机", "Edit host"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_ssh_editor(Some(edit_host.clone()), window, cx);
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("ssh-pin-{ix}")))
                        .icon(Icon::default().path(crate::gpui_shell::assets::nav::PIN))
                        .ghost()
                        .small()
                        .selected(pinned)
                        .toggled(pinned)
                        .tooltip(if pinned {
                            language.pick("取消置顶", "Unpin")
                        } else {
                            language.pick("置顶", "Pin")
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.ssh_apply(
                                |lists| lists.toggle_pin(&pin_host),
                                SshStatus::Pinned,
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("ssh-delete-{ix}")))
                        .map(|button| {
                            if confirm {
                                button
                                    .label(language.pick("确认删除", "Confirm delete"))
                                    .danger()
                                    .small()
                            } else {
                                button
                                    .icon(IconName::Delete)
                                    .ghost()
                                    .small()
                                    .tooltip(if from_config {
                                        language.tr("settings.ssh.hide_config_host")
                                    } else {
                                        language.pick("删除", "Delete")
                                    })
                            }
                        })
                        // 进了确认态就常显：指针移开还让它隐形，等于把「再点
                        // 一次才真删」这个状态藏起来。
                        .when(!confirm, |button| {
                            button
                                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.ssh_delete_confirm.as_deref() == Some(delete_host.as_str()) {
                                this.delete_ssh_host(&delete_host, cx);
                            } else {
                                this.ssh_delete_confirm = Some(delete_host.clone());
                                cx.notify();
                            }
                        })),
                )

        .into_any_element()
    }

    pub(in crate::gpui_shell) fn section_ssh(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let language = crate::gpui_shell::config::ui_language(cx);
        let controls = self.library_controls(cx);
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let hosts = self.filtered_library_hosts(cx);
        let host_count = hosts.len();
        let labels = self.ssh_hosts.profiles.labels();
        let icons = self.ssh_hosts.profiles.icons();
        let hidden: Vec<String> = self.ssh_hosts.hidden_hosts().to_vec();

        let row_hosts = hosts.clone();
        let host_rows = gpui::uniform_list(
            "ssh-library-hosts",
            host_count,
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|index| {
                        this.render_library_host(
                            row_hosts[index].clone(),
                            index,
                            host_count,
                            &labels,
                            &icons,
                            cx,
                        )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.ssh_library.scroll)
        .w_full()
        .h(px(SSH_HOST_ROW_H * host_count.clamp(1, 8) as f32));

        let hidden_rows = self.ssh_show_hidden.then(|| {
            hidden
                .iter()
                .enumerate()
                .map(|(ix, host)| {
                    let restore_host = host.clone();
                    h_flex()
                        .h(px(32.0))
                        .w_full()
                        .px_3()
                        .items_center()
                        .gap_2()
                        .when(ix + 1 < hidden.len(), |row| {
                            row.border_b_1().border_color(theme.border.opacity(0.5))
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .text_color(muted)
                                .truncate()
                                .child(host.clone()),
                        )
                        .child(
                            NebulaButton::new(SharedString::from(format!("ssh-restore-{ix}")))
                                .label(language.pick("恢复", "Restore"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.ssh_apply(
                                        |lists| lists.restore_hidden(&restore_host),
                                        SshStatus::Restored(restore_host.clone()),
                                        cx,
                                    );
                                })),
                        )
                })
                .collect::<Vec<_>>()
        });

        let hidden_count = self.ssh_hosts.hidden_hosts().len();
        let undo_bar = self.ssh_delete_undo.as_ref().map(|undo| (undo.host.clone(), undo.seq));

        self.group(language.pick("SSH 主机", "SSH hosts"), cx)
            .child(controls)
            .children(
                self.ssh_hosts
                    .load_error
                    .as_ref()
                    .map(|error| div().text_sm().text_color(theme.danger).child(error.clone())),
            )
            .child(
                h_flex()
                    .h(px(32.0))
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_color(theme.foreground)
                            .child(language.pick("已保存主机", "Saved hosts")),
                    )
                    .when(host_count > 0, |header| {
                        header.child(
                            div()
                                .px(px(6.0))
                                .rounded_sm()
                                .text_xs()
                                .text_color(muted)
                                .bg(theme.muted)
                                .child(SharedString::from(host_count.to_string())),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        NebulaButton::new("ssh-add-host")
                            .label(language.pick("+ 添加主机", "+ Add host"))
                            .primary()
                            .disabled(self.ssh_library.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_ssh_editor(None, window, cx);
                            })),
                    ),
            )
            .child(div().h(px(SSH_HOST_GAP)))
            .child(
                v_flex()
                    .w_full()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .when(host_count > 0, |card| card.child(host_rows))
                    .when(host_count == 0, |card| {
                        card.child(
                            v_flex()
                                .py_6()
                                .gap_1()
                                .items_center()
                                .child(
                                    div()
                                        .text_color(muted)
                                        .child(language.text(Message::HostsNoMatches)),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(language.tr("settings.ssh.empty_hint")),
                                ),
                        )
                    }),
            )
            .child(div().h(px(SSH_HOST_GAP)))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        NebulaButton::new("ssh-import")
                            .label(language.pick("导入 ~/.ssh/config", "Import ~/.ssh/config"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.reload_host_library(cx);
                            })),
                    )
                    .when(hidden_count > 0, |row| {
                        let show = self.ssh_show_hidden;
                        row.child(
                            NebulaButton::new("ssh-toggle-hidden")
                                .label(if show {
                                    SharedString::from(
                                        language.pick("收起已隐藏", "Collapse hidden"),
                                    )
                                } else {
                                    SharedString::from(format!(
                                        "{} {hidden_count}",
                                        language.pick("已隐藏", "Hidden")
                                    ))
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.ssh_show_hidden = !this.ssh_show_hidden;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(language.tr("settings.ssh.shared_data_hint")),
                    ),
            )
            .when_some(hidden_rows, |group, rows| {
                group.child(div().h(px(8.0))).child(
                    v_flex()
                        .w_full()
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme.border)
                        .overflow_hidden()
                        .children(rows),
                )
            })
            .when_some(undo_bar, |group, (host, _)| {
                group.child(div().h(px(8.0))).child(
                    h_flex()
                        .h(px(36.0))
                        .px_3()
                        .items_center()
                        .gap_2()
                        .rounded(px(6.0))
                        .bg(theme.muted)
                        .child(Icon::new(IconName::Undo2).xsmall().text_color(muted))
                        .child(div().flex_1().text_sm().child(SharedString::from(format!(
                            "{} {host}; {} {SSH_DELETE_UNDO_SECS} {}",
                            language.pick("已删除", "Deleted"),
                            language.pick("可在", "undo within"),
                            language.pick("秒", "seconds")
                        ))))
                        .child(
                            NebulaButton::new("ssh-undo-delete")
                                .label(language.pick("撤销", "Undo"))
                                .outline()
                                .on_click(cx.listener(|this, _, _, cx| this.undo_ssh_delete(cx))),
                        ),
                )
            })
            .when_some(self.ssh_status.clone(), |group, status| {
                let error = status.is_error();
                let message = status.text(language);
                group.child(
                    div()
                        .pt(px(6.0))
                        .text_sm()
                        .text_color(if error { theme.danger } else { theme.success })
                        .child(message),
                )
            })
    }
}
