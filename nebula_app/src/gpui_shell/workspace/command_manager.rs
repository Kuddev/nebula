//! 用户保存命令的右侧覆盖面板。
//!
//! 顶栏闪电列表只负责打开管理器；行内动作才表达执行、复制、编辑和删除。
//! 面板覆盖在终端之上而不参与主布局，避免为了短时管理命令永久压缩 PTY。

use super::*;

const PANEL_MAX_WIDTH: f32 = 480.0;
const PANEL_MARGIN: f32 = 8.0;
// 覆盖层从自绘标题栏下沿开始；固定组件依赖当前将该区域定义为 34px。
const WINDOW_TITLE_BAR_HEIGHT: f32 = 34.0;
const PANEL_HEADER_HEIGHT: f32 = 48.0;
const ROW_HEIGHT: f32 = 62.0;
const EDITOR_DIALOG_HEIGHT: f32 = 430.0;
const DELETE_DIALOG_HEIGHT: f32 = 230.0;
const MAX_SEARCH_BYTES: usize = 2 * 1024;
const COMMAND_MANAGER_KEY_CONTEXT: &str = "NebulaSavedCommands";

fn command_manager_icon() -> Icon {
    Icon::new(Icon::empty()).path(crate::gpui_shell::assets::nav::COMMAND_MANAGER)
}

fn custom_icon(path: &'static str) -> Icon {
    Icon::new(Icon::empty()).path(path)
}

/// 多行文本直接逐行送进 PTY 时，前台程序可能把第二行当成自己的 stdin。
/// 立即执行因此折成一条 shell 语句；仅插入模式保留用户原文供手动编辑。
fn dispatch_text(command: &crate::saved_commands::SavedCommand) -> String {
    if !command.append_enter || (!command.command.contains('\r') && !command.command.contains('\n'))
    {
        return command.command.clone();
    }
    command
        .command
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

fn word_start(text: &str, needle: &str) -> bool {
    text.split(|ch: char| !ch.is_alphanumeric()).any(|word| word.starts_with(needle))
}

fn field_score(text: &str, needle: &str, exact: i32, prefix: i32, word: i32, part: i32) -> i32 {
    if text == needle {
        exact
    } else if text.starts_with(needle) {
        prefix
    } else if word_start(text, needle) {
        word
    } else if text.contains(needle) {
        part
    } else {
        -1
    }
}

/// 名称匹配始终排在命令正文匹配之前；同分保持用户保存顺序。
fn search_score(command: &crate::saved_commands::SavedCommand, query: &str) -> Option<i32> {
    let name = command.name.to_lowercase();
    let body = command.command.to_lowercase();
    let mut total = 0;
    let mut matched = false;
    for word in query.split_whitespace() {
        let name_score = field_score(&name, word, 1_000, 850, 700, 550);
        let body_score = field_score(&body, word, 400, 340, 280, 220);
        let score = name_score.max(body_score);
        if score < 0 {
            return None;
        }
        matched = true;
        total += score;
    }
    matched.then_some(total)
}

impl NebulaWorkspace {
    fn filtered_saved_commands(&self, cx: &App) -> Vec<crate::saved_commands::SavedCommand> {
        let query = self.command_manager_input.read(cx).value().trim().to_lowercase();
        if query.is_empty() {
            return self.saved_commands.commands().to_vec();
        }
        if query.len() > MAX_SEARCH_BYTES {
            return Vec::new();
        }

        let mut matches = self
            .saved_commands
            .commands()
            .iter()
            .cloned()
            .enumerate()
            .filter_map(|(index, command)| {
                search_score(&command, &query).map(|score| (score, index, command))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|(left_score, left_index, _), (right_score, right_index, _)| {
            right_score.cmp(left_score).then_with(|| left_index.cmp(right_index))
        });
        matches.into_iter().map(|(_, _, command)| command).collect()
    }

    pub(super) fn toggle_command_manager(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.command_manager_open {
            self.close_command_manager(window, cx);
            return;
        }
        self.dismiss_palette_state();
        if let Err(error) = self.saved_commands.reload() {
            crate::gpui_shell::toast::toast(
                window,
                cx,
                crate::display::ToastKind::Warning,
                format!("无法读取已保存命令：{error}"),
            );
        }
        self.command_manager_open = true;
        self.command_manager_selected = 0;
        self.command_manager_scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        self.command_manager_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn close_command_manager(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.command_manager_open {
            return;
        }
        self.command_manager_open = false;
        self.focus_active(window, cx);
        cx.notify();
    }

    fn focus_command_manager_or_terminal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.command_manager_open {
            self.command_manager_input.update(cx, |input, cx| input.focus(window, cx));
        } else {
            self.focus_active(window, cx);
        }
    }

    fn move_saved_command_selection(&mut self, delta: isize, cx: &mut Context<'_, Self>) {
        if !self.command_manager_open {
            return;
        }
        let len = self.filtered_saved_commands(cx).len();
        self.command_manager_selected = if len == 0 {
            0
        } else {
            (self.command_manager_selected as isize + delta).rem_euclid(len as isize) as usize
        };
        self.command_manager_scroll.scroll_to_item(self.command_manager_selected);
        cx.notify();
    }

    pub(super) fn run_selected_saved_command(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let command = self.filtered_saved_commands(cx).get(self.command_manager_selected).cloned();
        if let Some(command) = command {
            self.dispatch_saved_command(command, window, cx);
        }
    }

    fn dispatch_saved_command(
        &mut self,
        command: crate::saved_commands::SavedCommand,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let view = self.tabs.get(self.active).and_then(WorkspaceTab::focused_view).cloned();
        let Some(view) = view else {
            crate::gpui_shell::toast::toast(
                window,
                cx,
                crate::display::ToastKind::Warning,
                "当前标签不是可用的终端",
            );
            return;
        };

        let submit = command.append_enter;
        let text = dispatch_text(&command);
        match view.update(cx, |view, cx| view.runtime_prompt(text, submit, cx)) {
            Ok(()) => {
                self.command_manager_open = false;
                self.focus_active(window, cx);
                cx.notify();
            },
            Err(error) => crate::gpui_shell::toast::toast(
                window,
                cx,
                crate::display::ToastKind::Warning,
                format!("无法发送命令：{}", error.message),
            ),
        }
    }

    fn copy_saved_command(
        &mut self,
        command: &crate::saved_commands::SavedCommand,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        cx.write_to_clipboard(ClipboardItem::new_string(command.command.clone()));
        crate::gpui_shell::toast::toast(window, cx, crate::display::ToastKind::Info, "命令已复制");
    }

    fn open_saved_command_editor(
        &mut self,
        edit_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let current = edit_id.as_deref().and_then(|id| {
            self.saved_commands.commands().iter().find(|command| command.id == id).cloned()
        });
        if edit_id.is_some() && current.is_none() {
            crate::gpui_shell::toast::toast(
                window,
                cx,
                crate::display::ToastKind::Warning,
                "这条命令已不存在",
            );
            return;
        }

        let language = workspace_ui_language();
        let initial_name = current.as_ref().map(|command| command.name.clone()).unwrap_or_default();
        let initial_command =
            current.as_ref().map(|command| command.command.clone()).unwrap_or_default();
        let append_enter = Rc::new(std::cell::Cell::new(
            current.as_ref().is_none_or(|command| command.append_enter),
        ));
        let name_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(language.pick("例如：启动开发服务", "For example: Start dev server"))
        });
        name_input.update(cx, |input, cx| input.set_value(initial_name, window, cx));
        let command_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .soft_wrap(true)
                .placeholder(language.pick("输入 Shell 命令", "Enter a shell command"))
        });
        command_input.update(cx, |input, cx| input.set_value(initial_command, window, cx));

        let workspace = cx.entity().downgrade();
        let dialog_workspace = workspace.clone();
        let dialog_name = name_input.clone();
        let dialog_command = command_input.clone();
        let dialog_append = append_enter.clone();
        let title = if edit_id.is_some() {
            language.pick("编辑命令", "Edit Command")
        } else {
            language.pick("新增命令", "Add Command")
        };
        let save_label = language.pick("保存", "Save");
        let cancel_label = language.pick("取消", "Cancel");

        window.open_dialog(cx, move |dialog, window, cx| {
            let checkbox_state = dialog_append.clone();
            let name = dialog_name.clone();
            let command = dialog_command.clone();
            let save_name = name.clone();
            let save_command = command.clone();
            let save_append = dialog_append.clone();
            let save_workspace = dialog_workspace.clone();
            let save_id = edit_id.clone();
            let close_workspace = dialog_workspace.clone();
            let body = v_flex()
                .w_full()
                .gap_3()
                .child(
                    v_flex()
                        .w_full()
                        .gap_1()
                        .child(div().text_sm().font_semibold().child(language.pick("名称", "Name")))
                        .child(Input::new(&name).w_full()),
                )
                .child(
                    v_flex()
                        .w_full()
                        .gap_1()
                        .child(
                            div().text_sm().font_semibold().child(language.pick("命令", "Command")),
                        )
                        .child(
                            div().w_full().h(px(150.0)).child(
                                Input::new(&command)
                                    .w_full()
                                    .font_family(cx.theme().mono_font_family.clone()),
                            ),
                        ),
                )
                .child(
                    gpui_component::checkbox::Checkbox::new("saved-command-append-enter")
                        .checked(checkbox_state.get())
                        .label(
                            language.pick(
                                "插入后立即按 Enter 执行",
                                "Press Enter and run after inserting",
                            ),
                        )
                        .on_click(move |checked, window, _| {
                            checkbox_state.set(*checked);
                            window.refresh();
                        }),
                )
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                    language.pick(
                        "关闭时只把命令放入当前终端，适合运行前补参数。",
                        "When off, the command is inserted for editing.",
                    ),
                ));
            let footer = DialogFooter::new()
                .child(div().flex_1())
                .child(
                    DialogClose::new()
                        .child(Button::new("saved-command-cancel").label(cancel_label)),
                )
                .child(
                    DialogAction::new()
                        .child(Button::new("saved-command-save").label(save_label).primary()),
                );

            center_modal_dialog(dialog, window, EDITOR_DIALOG_HEIGHT)
                .close_button(false)
                .overlay_closable(true)
                .title(div().text_lg().font_semibold().child(title))
                .footer(footer)
                .child(body)
                .on_ok(move |_, window, cx| {
                    let name = save_name.read(cx).value().to_string();
                    let command = save_command.read(cx).value().to_string();
                    let append_enter = save_append.get();
                    let Some(workspace) = save_workspace.upgrade() else {
                        return true;
                    };
                    let result = workspace.update(cx, |workspace, cx| {
                        let result = match save_id.as_deref() {
                            Some(id) => {
                                workspace.saved_commands.update(id, &name, &command, append_enter)
                            },
                            None => workspace
                                .saved_commands
                                .insert(&name, &command, append_enter)
                                .map(|_| ()),
                        };
                        if result.is_ok() {
                            cx.notify();
                        }
                        result
                    });
                    match result {
                        Ok(()) => {
                            crate::gpui_shell::toast::toast(
                                window,
                                cx,
                                crate::display::ToastKind::Success,
                                language.pick("命令已保存", "Command saved"),
                            );
                            true
                        },
                        Err(error) => {
                            crate::gpui_shell::toast::toast(
                                window,
                                cx,
                                crate::display::ToastKind::Warning,
                                format!("{}: {error}", language.pick("保存失败", "Save failed")),
                            );
                            false
                        },
                    }
                })
                .on_close(move |_, window, cx| {
                    if let Some(workspace) = close_workspace.upgrade() {
                        workspace.update(cx, |workspace, cx| {
                            workspace.focus_command_manager_or_terminal(window, cx);
                        });
                    }
                })
        });
        name_input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn open_delete_saved_command_dialog(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(command) =
            self.saved_commands.commands().iter().find(|command| command.id == id).cloned()
        else {
            return;
        };
        let language = workspace_ui_language();
        let workspace = cx.entity().downgrade();
        let dialog_workspace = workspace.clone();
        let command_name = command.name.clone();
        window.open_dialog(cx, move |dialog, window, cx| {
            let delete_workspace = dialog_workspace.clone();
            let close_workspace = dialog_workspace.clone();
            let delete_id = id.clone();
            let body = v_flex()
                .w_full()
                .gap_2()
                .child(div().text_sm().child(format!(
                    "{}“{}”？",
                    language.pick("确定删除命令 ", "Delete command "),
                    command_name
                )))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(language.pick("删除后无法撤销。", "This action cannot be undone.")),
                );
            let footer = DialogFooter::new()
                .child(div().flex_1())
                .child(
                    DialogClose::new().child(
                        Button::new("saved-command-delete-cancel")
                            .label(language.pick("取消", "Cancel")),
                    ),
                )
                .child(
                    DialogAction::new().child(
                        Button::new("saved-command-delete-confirm")
                            .label(language.pick("删除", "Delete"))
                            .danger(),
                    ),
                );
            center_modal_dialog(dialog, window, DELETE_DIALOG_HEIGHT)
                .close_button(false)
                .overlay_closable(true)
                .title(
                    div()
                        .text_lg()
                        .font_semibold()
                        .child(language.pick("删除已保存命令", "Delete Saved Command")),
                )
                .footer(footer)
                .child(body)
                .on_ok(move |_, window, cx| {
                    let Some(workspace) = delete_workspace.upgrade() else {
                        return true;
                    };
                    let result = workspace.update(cx, |workspace, cx| {
                        let result = workspace.saved_commands.remove(&delete_id);
                        if result.is_ok() {
                            let len = workspace.filtered_saved_commands(cx).len();
                            workspace.command_manager_selected =
                                workspace.command_manager_selected.min(len.saturating_sub(1));
                            cx.notify();
                        }
                        result
                    });
                    match result {
                        Ok(()) => true,
                        Err(error) => {
                            crate::gpui_shell::toast::toast(
                                window,
                                cx,
                                crate::display::ToastKind::Warning,
                                format!("{}: {error}", language.pick("删除失败", "Delete failed")),
                            );
                            false
                        },
                    }
                })
                .on_close(move |_, window, cx| {
                    if let Some(workspace) = close_workspace.upgrade() {
                        workspace.update(cx, |workspace, cx| {
                            workspace.focus_command_manager_or_terminal(window, cx);
                        });
                    }
                })
        });
    }

    pub(super) fn render_command_manager(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> gpui::AnyElement {
        use crate::display::ui::tokens::{control, radius, space};

        let theme = cx.theme();
        let panel_bg = theme.popover;
        let surface_bg = theme.muted;
        let selected_bg = theme.list_active;
        let hover_bg = theme.list_hover;
        let foreground = theme.foreground;
        let muted = theme.muted_foreground;
        let border = theme.border;
        let accent = theme.primary;
        let mono_family = theme.mono_font_family.clone();
        let language = workspace_ui_language();
        let viewport = window.viewport_size();
        let panel_width =
            PANEL_MAX_WIDTH.min((f32::from(viewport.width) - PANEL_MARGIN * 2.0).max(0.0));
        let panel_height =
            (f32::from(viewport.height) - WINDOW_TITLE_BAR_HEIGHT - PANEL_MARGIN).max(0.0);

        let commands = self.filtered_saved_commands(cx);
        if self.command_manager_selected >= commands.len() {
            self.command_manager_selected = commands.len().saturating_sub(1);
        }
        let command_count = self.saved_commands.commands().len();
        let selected_index = self.command_manager_selected;
        let list_scrollable = commands.len() as f32 * ROW_HEIGHT > (panel_height - 104.0).max(0.0);

        let mut rows = Vec::with_capacity(commands.len());
        for (index, command) in commands.into_iter().enumerate() {
            let selected = index == selected_index;
            let hover_group = SharedString::from(format!("saved-command-row-hover-{index}"));
            let preview = command
                .command
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let mode_label = if command.append_enter {
                language.pick("运行", "Run")
            } else {
                language.pick("插入", "Insert")
            };
            let run_tooltip = if command.append_enter {
                language.pick("运行命令", "Run command")
            } else {
                language.pick("插入到当前终端", "Insert into current terminal")
            };
            let run_icon =
                if command.append_enter { IconName::Play } else { IconName::SquareTerminal };
            let run_command = command.clone();
            let row_command = command.clone();
            let copy_command = command.clone();
            let edit_id = command.id.clone();
            let delete_id = command.id.clone();

            rows.push(
                h_flex()
                    .id(SharedString::from(format!("saved-command-row-{index}")))
                    .group(hover_group.clone())
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(space::XS))
                    .px_2()
                    .when(list_scrollable, |row| row.pr(px(18.0)))
                    .rounded(px(radius::CONTROL))
                    .cursor_pointer()
                    .when(selected, |row| row.bg(selected_bg))
                    .when(!selected, |row| {
                        row.group_hover(hover_group.clone(), |row| row.bg(hover_bg))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.command_manager_selected = index;
                        this.dispatch_saved_command(row_command.clone(), window, cx);
                    }))
                    .child(
                        Button::new(SharedString::from(format!("saved-command-run-{index}")))
                            .icon(run_icon)
                            .ghost()
                            .xsmall()
                            .tooltip(run_tooltip)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.command_manager_selected = index;
                                this.dispatch_saved_command(run_command.clone(), window, cx);
                            })),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(space::XXS))
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(foreground)
                                            .child(command.name),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .rounded(px(radius::CHIP))
                                            .border_1()
                                            .border_color(border)
                                            .px_1()
                                            .text_size(px(10.0))
                                            .text_color(if selected { accent } else { muted })
                                            .child(mode_label),
                                    ),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(mono_family.clone())
                                    .text_size(px(11.0))
                                    .text_color(muted)
                                    .child(preview),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap_1()
                            .child(
                                Button::new(SharedString::from(format!(
                                    "saved-command-copy-{index}"
                                )))
                                .icon(IconName::Copy)
                                .ghost()
                                .xsmall()
                                .tooltip(language.pick("复制命令", "Copy command"))
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.copy_saved_command(&copy_command, window, cx);
                                    },
                                )),
                            )
                            .child(
                                Button::new(SharedString::from(format!(
                                    "saved-command-edit-{index}"
                                )))
                                .icon(custom_icon(crate::gpui_shell::assets::nav::PENCIL))
                                .ghost()
                                .xsmall()
                                .tooltip(language.pick("编辑命令", "Edit command"))
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.open_saved_command_editor(
                                            Some(edit_id.clone()),
                                            window,
                                            cx,
                                        );
                                    },
                                )),
                            )
                            .child(
                                Button::new(SharedString::from(format!(
                                    "saved-command-delete-{index}"
                                )))
                                .icon(custom_icon(crate::gpui_shell::assets::nav::TRASH))
                                .ghost()
                                .xsmall()
                                .tooltip(language.pick("删除命令", "Delete command"))
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.open_delete_saved_command_dialog(
                                            delete_id.clone(),
                                            window,
                                            cx,
                                        );
                                    },
                                )),
                            ),
                    )
                    .into_any_element(),
            );
        }

        let search_box = h_flex()
            .w_full()
            .h(px(control::MIN_HIT_TARGET))
            .flex_shrink_0()
            .rounded(px(radius::CONTROL))
            .border_1()
            .border_color(border)
            .bg(surface_bg)
            .overflow_hidden()
            .child(
                Input::new(&self.command_manager_input)
                    .w_full()
                    .appearance(false)
                    .focus_bordered(false)
                    .cleanable(true)
                    .prefix(Icon::new(IconName::Search).xsmall().text_color(muted))
                    .text_size(px(13.0)),
            );

        let list_content = if rows.is_empty() {
            let has_saved_commands = command_count > 0;
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(muted)
                .child(command_manager_icon().with_size(px(28.0)))
                .child(div().text_sm().text_color(foreground).child(if has_saved_commands {
                    language.pick("没有匹配的命令", "No matching commands")
                } else {
                    language.pick("还没有保存命令", "No saved commands yet")
                }))
                .when(!has_saved_commands, |empty| {
                    empty.child(
                        Button::new("saved-command-empty-add")
                            .icon(IconName::Plus)
                            .label(language.pick("新增命令", "Add command"))
                            .primary()
                            .on_click(cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.open_saved_command_editor(None, window, cx);
                            })),
                    )
                })
                .into_any_element()
        } else {
            let scroll_handle = self.command_manager_scroll.clone();
            let list = v_flex()
                .id("saved-command-results-scroll")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&scroll_handle)
                .children(rows);
            div()
                .relative()
                .size_full()
                .min_h_0()
                .overflow_hidden()
                .child(list)
                .when(list_scrollable, |list| {
                    list.child(
                        gpui_component::scroll::Scrollbar::vertical(&scroll_handle)
                            .scrollbar_show(gpui_component::scroll::ScrollbarShow::Always),
                    )
                })
                .into_any_element()
        };

        div()
            .absolute()
            .inset_0()
            .occlude()
            .key_context(COMMAND_MANAGER_KEY_CONTEXT)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.close_command_manager(window, cx);
                }),
            )
            .on_key_down(cx.listener(|this: &mut Self, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "up" => {
                        this.move_saved_command_selection(-1, cx);
                        cx.stop_propagation();
                    },
                    "down" => {
                        this.move_saved_command_selection(1, cx);
                        cx.stop_propagation();
                    },
                    "escape" => {
                        this.close_command_manager(window, cx);
                        cx.stop_propagation();
                    },
                    _ => {},
                }
            }))
            .child(
                v_flex()
                    .absolute()
                    .top(px(WINDOW_TITLE_BAR_HEIGHT))
                    .right(px(PANEL_MARGIN))
                    .w(px(panel_width))
                    .h(px(panel_height))
                    .rounded(px(radius::OVERLAY))
                    .border_1()
                    .border_color(border)
                    .bg(panel_bg)
                    .shadow_lg()
                    .overflow_hidden()
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _, _, cx| cx.stop_propagation()),
                    )
                    .child(
                        h_flex()
                            .h(px(PANEL_HEADER_HEIGHT))
                            .w_full()
                            .flex_shrink_0()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .border_b_1()
                            .border_color(border)
                            .child(command_manager_icon().small().text_color(accent))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(foreground)
                                    .child(language.pick("命令列表", "Command List")),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .font_family(mono_family)
                                    .text_size(px(11.0))
                                    .text_color(muted)
                                    .child(command_count.to_string()),
                            )
                            .child(div().flex_1())
                            .child(
                                Button::new("saved-command-add")
                                    .icon(IconName::Plus)
                                    .ghost()
                                    .xsmall()
                                    .tooltip(language.pick("新增命令", "Add command"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.open_saved_command_editor(None, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("saved-command-close")
                                    .icon(IconName::Close)
                                    .ghost()
                                    .xsmall()
                                    .tooltip(language.pick("关闭命令列表", "Close command list"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.close_command_manager(window, cx);
                                    })),
                            ),
                    )
                    .child(div().w_full().flex_shrink_0().p_2().child(search_box))
                    .child(div().flex_1().min_h_0().px_2().pb_2().child(list_content)),
            )
            .into_any_element()
    }
}
