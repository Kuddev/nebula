//! 终端选区工作流：右键菜单 -> 引用/评论 -> 指定 Agent pane。
//!
//! 对话框复用 Nebula 的标准模态几何与 runtime prompt 合同；消息使用明确的
//! Markdown blockquote 兼容格式，目标 pane 必须是仍在前台的 Agent，且必须
//! 处于 bracketed-paste 模式，否则拒绝发送（见 `runtime_chat_message`）。
//!
//! 安全边界分三层，只有最后一层是强制的：
//!
//! 1. **便利层**（本文件）：目标列表只列出真的有 Agent 在前台的 pane，远端目标
//!    额外挂上主机名徽标——选区发过去就离开了本机，这件事必须在选之前看得见；
//! 2. **自查层**（`ssh_session`）：远端 pane 的环境里带 `NEBULA_PANE_REMOTE=1`，
//!    愿意自查的被调方可以自己拒绝；
//! 3. **强制层**（`TerminalView::ensure_local_context_allowed`）：判据是 pane
//!    自己的身份，与调用方声明了什么无关。用户当场选中远端 pane 放行，程序
//!    自动把本地内容送去远端一律拒绝。

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    Anchor, AnyElement, App, AppContext as _, ClipboardItem, Context, DismissEvent, Entity,
    Focusable as _, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Point,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, anchored,
    deferred, div, px, relative,
};
use gpui_component::menu::PopupMenuItem;
use gpui_component::select::SelectItem;

use crate::gpui_shell::prelude::*;
use crate::gpui_shell::terminal::view::{InputOrigin, TerminalView};

use super::{NebulaWorkspace, WorkspaceTab, workspace_ui_language};

const SEND_TO_CHAT_DIALOG_HEIGHT: f32 = 510.0;
const QUOTE_PREVIEW_HEIGHT: f32 = 132.0;
const COMMENT_HEIGHT: f32 = 112.0;

pub(super) struct SelectionContextMenu {
    menu: Entity<PopupMenu>,
    position: Point<Pixels>,
    _subscription: Subscription,
}

#[derive(Clone)]
struct SendToChatTarget {
    pane_id: u64,
    title: SharedString,
    agent: SharedString,
    /// 目标是远端主机时的 destination。护栏的第一层：选区一旦发过去就离开了
    /// 本机，用户必须在选之前就看见这件事。
    remote: Option<SharedString>,
    search: String,
}

impl SelectItem for SendToChatTarget {
    type Value = u64;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn display_title(&self) -> Option<AnyElement> {
        Some(
            h_flex()
                .min_w_0()
                .gap_2()
                .child(Icon::new(IconName::Star).xsmall())
                .child(div().min_w_0().truncate().child(self.title.clone()))
                .into_any_element(),
        )
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .h(px(42.0))
            .w_full()
            .min_w_0()
            .gap_2()
            .child(
                div()
                    .flex()
                    .size(px(24.0))
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .rounded(px(crate::display::ui::tokens::radius::CHIP))
                    .bg(cx.theme().group_box)
                    .child(Icon::new(IconName::Star).xsmall().text_color(cx.theme().link)),
            )
            .child(div().min_w_0().flex_1().truncate().child(self.title.clone()))
            // 远端目标必须在被选中之前就看得见：选区发过去就离开了本机。
            .children(self.remote.clone().map(|destination| {
                div()
                    .flex_shrink_0()
                    .px(px(6.0))
                    .rounded(px(crate::display::ui::tokens::radius::CHIP))
                    .bg(cx.theme().warning.opacity(0.16))
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child(destination)
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.agent.clone()),
            )
    }

    fn value(&self) -> &Self::Value {
        &self.pane_id
    }

    fn matches(&self, query: &str) -> bool {
        self.search.contains(&query.to_lowercase())
    }
}

impl NebulaWorkspace {
    pub(super) fn open_terminal_selection_context_menu(
        &mut self,
        source: Entity<TerminalView>,
        _source_pane_id: u64,
        position: Point<Pixels>,
        selection: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let language = workspace_ui_language();
        let source = source.downgrade();
        let copy_item = PopupMenuItem::new(language.pick("复制选区", "Copy Selection"))
            .icon(IconName::Copy)
            .on_click(move |_, window, cx| {
                if let Some(source) = source.upgrade() {
                    source.update(cx, |view, cx| {
                        view.copy_selection(true, window, cx);
                    });
                }
            });
        self.open_selection_context_menu(position, selection.into(), copy_item, window, cx);
    }

    pub(super) fn open_document_selection_context_menu(
        &mut self,
        position: Point<Pixels>,
        selection: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let language = workspace_ui_language();
        let selection: Arc<str> = selection.into();
        let copy_selection = selection.clone();
        let copy_item = PopupMenuItem::new(language.pick("复制选区", "Copy Selection"))
            .icon(IconName::Copy)
            .on_click(move |_, window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy_selection.to_string()));
                crate::gpui_shell::toast::toast(
                    window,
                    cx,
                    crate::display::ToastKind::Info,
                    language.pick("选区已复制", "Selection copied"),
                );
            });
        self.open_selection_context_menu(position, selection, copy_item, window, cx);
    }

    fn open_selection_context_menu(
        &mut self,
        position: Point<Pixels>,
        selection: Arc<str>,
        copy_item: PopupMenuItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let language = workspace_ui_language();
        let has_targets = !self.send_to_chat_targets(cx).is_empty();
        let workspace = cx.entity().downgrade();
        let menu = PopupMenu::build(window, cx, move |menu, _window, _cx| {
            let send_workspace = workspace.clone();
            let send_selection = selection.clone();
            menu.external_link_icon(false).item(copy_item).item(
                PopupMenuItem::new(language.pick("发送到聊天...", "Send to Chat..."))
                    // 当前图标包没有 speech-bubble；对 Agent 使用中性星形而非
                    // CLI 品牌图，与 Quick Jump 的会话行保持同一语义。
                    .icon(IconName::Star)
                    .disabled(!has_targets)
                    .on_click(move |_, window, cx| {
                        if let Some(workspace) = send_workspace.upgrade() {
                            workspace.update(cx, |workspace, cx| {
                                workspace.open_send_to_chat_dialog(
                                    send_selection.clone(),
                                    window,
                                    cx,
                                );
                            });
                        }
                    }),
            )
        });
        menu.focus_handle(cx).focus(window, cx);
        let subscription = cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, _, cx| {
            this.selection_context_menu = None;
            cx.notify();
        });
        self.selection_context_menu =
            Some(SelectionContextMenu { menu, position, _subscription: subscription });
        cx.notify();
    }

    pub(super) fn render_selection_context_menu(&self) -> Option<AnyElement> {
        let state = self.selection_context_menu.as_ref()?;
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

    fn send_to_chat_targets(&self, cx: &App) -> Vec<SendToChatTarget> {
        let mut targets = Vec::new();
        for (tab_ix, tab) in self.tabs.iter().enumerate() {
            let WorkspaceTab::Terminal { panes, .. } = tab else { continue };
            let tab_title = self.tab_title(tab_ix, cx);
            for pane in panes {
                let view = pane.view.read(cx);
                let Some(agent) = view.runtime_chat_agent() else { continue };
                let title: SharedString = if panes.len() == 1 {
                    tab_title.clone()
                } else {
                    format!("{} · {}", tab_title, view.title).into()
                };
                let agent_name: SharedString = agent.display_name.into();
                let remote: Option<SharedString> =
                    view.ssh_destination.clone().map(SharedString::from);
                targets.push(SendToChatTarget {
                    pane_id: pane.id,
                    search: format!(
                        "{title} {agent_name} {}",
                        remote.as_deref().unwrap_or_default()
                    )
                    .to_lowercase(),
                    title,
                    agent: agent_name,
                    remote,
                });
            }
        }
        targets
    }

    fn open_send_to_chat_dialog(
        &mut self,
        selection: Arc<str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let language = workspace_ui_language();
        let targets = self.send_to_chat_targets(cx);
        if targets.is_empty() {
            crate::gpui_shell::toast::toast(
                window,
                cx,
                crate::display::ToastKind::Warning,
                language.pick(
                    "当前没有可接收消息的 Agent pane",
                    "No Agent pane can receive this message",
                ),
            );
            return;
        }

        let target_count = targets.len();
        let target_labels: Arc<HashMap<u64, SharedString>> =
            Arc::new(targets.iter().map(|target| (target.pane_id, target.title.clone())).collect());
        let target_select =
            cx.new(|cx| SelectState::new(targets, None, window, cx).searchable(target_count > 6));
        let comment = cx.new(|cx| InputState::new(window, cx).multi_line(true).soft_wrap(true));
        let workspace = cx.entity().downgrade();
        let quote_text: SharedString = selection.to_string().into();
        let dialog_target_select = target_select.clone();

        window.open_dialog(cx, move |dialog, window, cx| {
            let target_selected = dialog_target_select.read(cx).selected_value().is_some();
            let quote = div()
                .id("send-to-chat-quote")
                .flex()
                .h(px(QUOTE_PREVIEW_HEIGHT))
                .w_full()
                .overflow_hidden()
                .rounded(px(crate::display::ui::tokens::radius::CONTROL))
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().group_box)
                .child(div().w(px(3.0)).h_full().flex_shrink_0().bg(cx.theme().link))
                .child(
                    div()
                        .id("send-to-chat-quote-scroll")
                        .min_w_0()
                        .h_full()
                        .flex_1()
                        .overflow_y_scroll()
                        .px_3()
                        .py_2()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_sm()
                        .line_height(relative(1.45))
                        .text_color(cx.theme().foreground)
                        .child(quote_text.clone()),
                );

            let body = v_flex()
                .w_full()
                .gap_3()
                .child(
                    v_flex()
                        .w_full()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_semibold()
                                .child(language.pick("发送到：", "Send to:")),
                        )
                        .child(
                            div().w_full().h_8().child(
                                Select::new(&dialog_target_select)
                                    .placeholder(
                                        language.pick("选择 Agent...", "Choose an Agent..."),
                                    )
                                    .search_placeholder(
                                        language.pick("搜索 Agent...", "Search Agents..."),
                                    ),
                            ),
                        ),
                )
                .child(quote)
                .child(
                    v_flex()
                        .w_full()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_semibold()
                                .child(language.pick("评论：", "Comment:")),
                        )
                        .child(div().w_full().h(px(COMMENT_HEIGHT)).child(Input::new(&comment))),
                );

            let copy_selection = selection.clone();
            let copy_comment = comment.clone();
            let copy_text = language.pick("复制消息", "Copy Message");
            let cancel_text = language.pick("取消", "Cancel");
            let send_text = language.pick("发送", "Send");
            let footer = DialogFooter::new()
                .child(
                    Button::new("send-to-chat-copy")
                        .label(copy_text)
                        .icon(IconName::Copy)
                        .ghost()
                        .on_click(move |_, window, cx| {
                            let comment = copy_comment.read(cx).value().to_string();
                            let message = compose_send_to_chat_message(&copy_selection, &comment);
                            cx.write_to_clipboard(ClipboardItem::new_string(message));
                            crate::gpui_shell::toast::toast(
                                window,
                                cx,
                                crate::display::ToastKind::Info,
                                language.pick("消息已复制", "Message copied"),
                            );
                        }),
                )
                .child(div().flex_1())
                .child(
                    DialogClose::new().child(Button::new("send-to-chat-cancel").label(cancel_text)),
                )
                .child(
                    DialogAction::new().child(
                        Button::new("send-to-chat-send")
                            .label(send_text)
                            .primary()
                            .disabled(!target_selected),
                    ),
                );

            let send_workspace = workspace.clone();
            let close_workspace = workspace.clone();
            let send_selection = selection.clone();
            let send_comment = comment.clone();
            let send_target = dialog_target_select.clone();
            let send_labels = target_labels.clone();
            center_modal_dialog(dialog, window, SEND_TO_CHAT_DIALOG_HEIGHT)
                .close_button(false)
                .overlay_closable(true)
                .title(
                    div()
                        .text_lg()
                        .font_semibold()
                        .line_height(relative(1.0))
                        .child(language.pick("发送到对话", "Send to Chat")),
                )
                .footer(footer)
                .child(body)
                .on_ok(move |_, window, cx| {
                    let Some(pane_id) = send_target.read(cx).selected_value().copied() else {
                        crate::gpui_shell::toast::toast(
                            window,
                            cx,
                            crate::display::ToastKind::Warning,
                            language.pick("请选择一个 Agent pane", "Choose an Agent pane"),
                        );
                        return false;
                    };
                    let comment = send_comment.read(cx).value().to_string();
                    let message = compose_send_to_chat_message(&send_selection, &comment);
                    let Some(workspace) = send_workspace.upgrade() else { return true };
                    match workspace.update(cx, |workspace, cx| {
                        workspace.send_to_chat_prompt(pane_id, message, cx)
                    }) {
                        Ok(()) => {
                            let target = send_labels
                                .get(&pane_id)
                                .cloned()
                                .unwrap_or_else(|| pane_id.to_string().into());
                            crate::gpui_shell::toast::toast(
                                window,
                                cx,
                                crate::display::ToastKind::Info,
                                match language {
                                    crate::display::UiLanguage::EnUs => {
                                        format!("Sent to {target}")
                                    },
                                    _ => format!("已发送到 {target}"),
                                },
                            );
                            true
                        },
                        Err(error) => {
                            crate::gpui_shell::toast::toast(
                                window,
                                cx,
                                crate::display::ToastKind::Warning,
                                error.message,
                            );
                            false
                        },
                    }
                })
                .on_close(move |_, window, cx| {
                    if let Some(workspace) = close_workspace.upgrade() {
                        workspace.update(cx, |workspace, cx| workspace.focus_active(window, cx));
                    }
                })
        });
        let target_focus = target_select.focus_handle(cx);
        target_focus.focus(window, cx);
    }

    fn send_to_chat_prompt(
        &mut self,
        pane_id: u64,
        message: String,
        cx: &mut Context<Self>,
    ) -> Result<(), crate::runtime_api::ApiError> {
        let Some(tab_ix) = self.tab_of_pane(pane_id) else {
            return Err(crate::runtime_api::ApiError::new(
                "target_not_found",
                format!("pane {pane_id} does not exist"),
            ));
        };
        let view = match self.tabs.get(tab_ix) {
            Some(WorkspaceTab::Terminal { panes, .. }) => {
                panes.iter().find(|pane| pane.id == pane_id).map(|pane| pane.view.clone())
            },
            _ => None,
        }
        .expect("tab_of_pane resolved a terminal pane");
        // 用户在对话框里当场选定了这个 pane，属于知情选择：远端目标也放行，
        // 但对话框必须已经把它标成远端主机（见 `send_to_chat_targets`）。
        view.update(cx, |view, cx| view.runtime_chat_message(message, InputOrigin::User, cx))
    }
}

fn compose_send_to_chat_message(selection: &str, comment: &str) -> String {
    let normalized = selection.replace("\r\n", "\n").replace('\r', "\n");
    let selection = normalized.trim_end_matches('\n');
    let mut message = selection
        .split('\n')
        .map(|line| if line.is_empty() { ">".to_owned() } else { format!("> {line}") })
        .collect::<Vec<_>>()
        .join("\n");
    let comment = comment.trim();
    if !comment.is_empty() {
        message.push_str("\n\n");
        message.push_str(comment);
    }
    message
}

#[cfg(test)]
mod tests {
    use super::compose_send_to_chat_message;

    #[test]
    fn message_quotes_selection_and_appends_comment() {
        assert_eq!(
            compose_send_to_chat_message("alpha\r\n\r\nbeta\n", "  explain this  "),
            "> alpha\n>\n> beta\n\nexplain this"
        );
    }

    #[test]
    fn message_without_comment_is_only_the_quote() {
        assert_eq!(compose_send_to_chat_message("alpha", "  "), "> alpha");
    }
}
