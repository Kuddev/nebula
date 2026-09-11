//! Compact document chrome, separate from the buffer and document lifecycle.
use super::reader_presentation as design;
use super::*;

impl TextFileView {
    pub(super) fn render_toolbar(
        &self,
        editable: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let language = super::super::config::ui_language(cx);
        let muted = cx.theme().muted_foreground;
        let parent = self
            .path
            .parent()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        h_flex()
            .h(px(design::TOOLBAR_HEIGHT))
            .flex_shrink_0()
            .px(px(design::PANEL_PADDING))
            .gap(px(9.0))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(7.0))
                    .child(Icon::new(IconName::File).size(px(design::ICON_SIZE)).text_color(muted))
                    .when(!parent.is_empty(), |row| {
                        row.child(
                            div()
                                .max_w(px(140.0))
                                .truncate()
                                .text_size(px(design::SECONDARY_SIZE))
                                .text_color(muted)
                                .child(parent),
                        )
                        .child(
                            div()
                                .text_size(px(design::SECONDARY_SIZE))
                                .text_color(muted)
                                .child("/"),
                        )
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(design::CHROME_SIZE))
                            .font_semibold()
                            .child(self.title.clone()),
                    )
                    .when(self.dirty, |row| {
                        row.child(
                            div()
                                .size(px(5.0))
                                .flex_shrink_0()
                                .rounded_full()
                                .bg(cx.theme().warning),
                        )
                    }),
            )
            .when(self.markdown, |bar| bar.child(self.render_mode_switch(cx)))
            .child(div().w(px(1.0)).h(px(18.0)).mx_1().bg(cx.theme().border))
            .child(
                Button::new("file-details-toggle")
                    .ghost()
                    .size(px(design::CONTROL_HEIGHT))
                    .flex_shrink_0()
                    .icon(Icon::new(IconName::PanelRight).size(px(design::ICON_SIZE)))
                    .selected(self.show_details)
                    .tooltip(language.text(Message::EditorDetailsToggle))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_details = !this.show_details;
                        if !this.markdown {
                            this.info = true;
                        }
                        cx.notify();
                    })),
            )
            .when((self.loading || self.saving) && self.source.is_remote(), |bar| {
                bar.child(
                    Button::new("file-cancel-operation")
                        .ghost()
                        .xsmall()
                        .label(language.text(Message::TransferCancel))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(operation) = &this.operation {
                                operation.cancel();
                            }
                            this.notice = Some((Message::TransferCancelling, None));
                            cx.notify();
                        })),
                )
            })
            .child(
                Button::new("file-fullscreen")
                    .ghost()
                    .size(px(design::CONTROL_HEIGHT))
                    .flex_shrink_0()
                    .icon(
                        Icon::new(if self.markdown {
                            if self.reader_focus { IconName::Minimize } else { IconName::Maximize }
                        } else if window.is_fullscreen() {
                            IconName::Minimize
                        } else {
                            IconName::Maximize
                        })
                        .size(px(design::ICON_SIZE)),
                    )
                    .tooltip(language.text(if self.markdown {
                        if self.reader_focus {
                            Message::EditorExitFocusReading
                        } else {
                            Message::EditorFocusReading
                        }
                    } else if window.is_fullscreen() {
                        Message::EditorExitFullscreen
                    } else {
                        Message::EditorFullscreen
                    }))
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.markdown {
                            this.toggle_reader_focus(cx);
                        } else {
                            window.toggle_fullscreen();
                            cx.notify();
                        }
                    })),
            )
            .child(
                Button::new("file-save")
                    .ghost()
                    .xsmall()
                    .h(px(design::CONTROL_HEIGHT))
                    .px(px(12.0))
                    .flex_shrink_0()
                    .text_size(px(design::CHROME_SIZE))
                    .label(language.text(if self.saving {
                        Message::EditorSaving
                    } else {
                        Message::EditorSave
                    }))
                    .tooltip(language.text(Message::EditorSaveShortcut))
                    .disabled(!editable || !self.dirty || self.saving)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.save(cx).detach();
                    })),
            )
            .into_any_element()
    }

    fn render_mode_switch(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let language = super::super::config::ui_language(cx);
        h_flex()
            .flex_shrink_0()
            .p(px(3.0))
            .gap(px(2.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .children(
                [
                    (true, "file-mode-read", Message::EditorReading),
                    (false, "file-mode-edit", Message::EditorEdit),
                ]
                .into_iter()
                .map(|(preview, id, label)| {
                    div().debug_selector(move || id.to_owned()).child(
                        Button::new(id)
                            .ghost()
                            .xsmall()
                            .h(px(design::CONTROL_HEIGHT))
                            .rounded(px(4.0))
                            .px(px(10.0))
                            .text_size(px(design::SECONDARY_SIZE))
                            .label(language.text(label))
                            .when(self.preview == preview, |button| {
                                button.bg(cx.theme().background).text_color(cx.theme().foreground)
                            })
                            .when(self.preview != preview, |button| {
                                button.text_color(cx.theme().muted_foreground)
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if this.preview != preview {
                                    this.toggle_preview(window, cx);
                                }
                            })),
                    )
                }),
            )
            .into_any_element()
    }

    pub(super) fn render_details_header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let language = super::super::config::ui_language(cx);
        h_flex()
            .h(px(design::PANEL_HEADER_HEIGHT))
            .flex_shrink_0()
            .px(px(design::PANEL_PADDING))
            .gap(px(20.0))
            .border_b_1()
            .border_color(cx.theme().border)
            .children(
                [
                    (false, "file-toggle-outline", Message::EditorOutline),
                    (true, "file-info", Message::EditorInfo),
                ]
                .into_iter()
                .filter(|(info, _, _)| *info || self.markdown)
                .map(|(info, id, label)| {
                    div()
                        .debug_selector(move || id.to_owned())
                        .relative()
                        .h_full()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .child(
                            Button::new(id)
                                .ghost()
                                .xsmall()
                                .h_full()
                                .min_w(px(48.0))
                                .px(px(8.0))
                                .rounded(px(0.0))
                                .text_size(px(design::SECONDARY_SIZE))
                                .label(language.text(label))
                                .text_color(if self.info == info {
                                    cx.theme().foreground
                                } else {
                                    cx.theme().muted_foreground
                                })
                                .when(self.info == info, |button| button.font_semibold())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.info = info;
                                    this.show_details = true;
                                    cx.notify();
                                })),
                        )
                        .when(self.info == info, |tab| {
                            tab.child(
                                div()
                                    .debug_selector(|| "file-details-indicator".to_owned())
                                    .absolute()
                                    .bottom_0()
                                    .left_0()
                                    .right_0()
                                    .h(px(2.0))
                                    .bg(cx.theme().foreground.opacity(0.55)),
                            )
                        })
                }),
            )
            .child(div().flex_1())
            .child(
                Button::new("file-details-close")
                    .ghost()
                    .size(px(design::CONTROL_HEIGHT))
                    .flex_shrink_0()
                    .icon(Icon::new(IconName::Close).size(px(design::ICON_SIZE)))
                    .tooltip(language.text(Message::EditorDetailsClose))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_details = false;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// An 8px hit target centered on the one-pixel panel divider. The actual
    /// panel width is shared by outline and file-info, so both views resize in
    /// exactly the same way.
    pub(super) fn render_details_resize_handle(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let hover_line = cx.theme().border.opacity(0.45);
        div()
            .id("file-details-resize-handle")
            .debug_selector(|| "file-details-resize-handle".to_owned())
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(-(design::DETAILS_RESIZE_HIT_WIDTH * 0.5)))
            .w(px(design::DETAILS_RESIZE_HIT_WIDTH))
            .cursor_col_resize()
            .hover(move |handle| handle.bg(hover_line))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event, _, cx| {
                    gpui_component::GlobalState::suppress_text_selection(cx);
                    this.begin_details_resize(event, cx);
                    cx.stop_propagation();
                }),
            )
            .into_any_element()
    }
}
