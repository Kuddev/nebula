use super::reader_presentation as design;
use super::*;

impl TextFileView {
    pub(super) fn render_info(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let language = super::super::config::ui_language(cx);
        let muted = cx.theme().muted_foreground;
        let text = self.input.read(cx).value();
        let kind = self.path.extension().unwrap_or_default().to_string_lossy().to_uppercase();
        let statistics = vec![
            (Message::EditorCharacters, text.chars().count().to_string()),
            (Message::EditorLineCount, text.lines().count().to_string()),
            (
                Message::EditorStatus,
                language
                    .text(if self.dirty { Message::EditorUnsaved } else { Message::EditorSaved })
                    .to_owned(),
            ),
        ];
        let mut properties = Vec::new();
        if let Some(document) = &self.document {
            properties.push((Message::EditorSize, format!("{} B", document.size())));
            properties.push((
                Message::EditorEncodingLabel,
                if document.invalid_encoding {
                    "—"
                } else if document.bom {
                    "UTF-8 BOM"
                } else {
                    "UTF-8"
                }
                .to_owned(),
            ));
            properties.push((
                Message::EditorLineEnding,
                if document.crlf { "CRLF" } else { "LF" }.to_owned(),
            ));
            if let Some(modified) = document.modified() {
                let date: chrono::DateTime<chrono::Local> = modified.into();
                properties
                    .push((Message::EditorModified, date.format("%Y-%m-%d %H:%M").to_string()));
            }
        }
        let section = |title, rows: Vec<(Message, String)>| {
            v_flex()
                .gap(px(4.0))
                .mt(px(24.0))
                .child(
                    div()
                        .mb(px(10.0))
                        .text_size(px(design::SECONDARY_SIZE))
                        .text_color(muted)
                        .child(language.text(title)),
                )
                .children(rows.into_iter().map(|(label, value)| {
                    h_flex()
                        .w_full()
                        .items_start()
                        .gap_2()
                        .py(px(5.0))
                        .text_size(px(design::SECONDARY_SIZE))
                        .child(div().flex_shrink_0().text_color(muted).child(language.text(label)))
                        .child(div().flex_1().min_w_0().text_right().child(value))
                }))
        };
        let path = self.source.display();
        let name = self.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let reveal = self.path.clone();
        let external = self.path.clone();
        let action = |id: &'static str, icon: IconName, label: Message| {
            Button::new(id).ghost().w_full().h(px(design::ACTION_HEIGHT)).px(px(6.0)).child(
                h_flex()
                    .debug_selector(move || id.to_owned())
                    .w_full()
                    .h_full()
                    .gap(px(10.0))
                    .text_size(px(design::SECONDARY_SIZE))
                    .text_color(muted)
                    .child(Icon::new(icon).size(px(design::ICON_SIZE)))
                    .child(language.text(label)),
            )
        };
        let actions = v_flex()
            .gap(px(3.0))
            .mt(px(24.0))
            .pt(px(14.0))
            .border_t_1()
            .border_color(cx.theme().border)
            .child(action("info-copy-path", IconName::Copy, Message::EditorCopyPath).on_click(
                move |_, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(path.clone()))
                },
            ))
            .child(action("info-copy-name", IconName::File, Message::EditorCopyName).on_click(
                move |_, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(name.clone()))
                },
            ))
            .when(!self.source.is_remote(), |panel| {
                panel
                    .child(
                        action("info-reveal", IconName::FolderOpen, Message::EditorReveal)
                            .on_click(move |_, _, _| {
                                super::super::workspace::reveal_in_file_manager(&reveal)
                            }),
                    )
                    .child(
                        action("info-open", IconName::ExternalLink, Message::EditorOpenExternal)
                            .on_click(move |_, _, _| {
                                super::super::workspace::open_in_file_manager(&external)
                            }),
                    )
            });
        let body = v_flex()
            .id("file-info-body")
            .flex_1()
            .min_h_0()
            .px(px(design::PANEL_PADDING))
            .py(px(22.0))
            .overflow_y_scroll()
            .child(
                h_flex()
                    .gap(px(10.0))
                    .items_center()
                    .child(
                        div()
                            .w(px(32.0))
                            .h(px(38.0))
                            .flex_shrink_0()
                            .rounded(px(5.0))
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().muted)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(10.0))
                            .font_semibold()
                            .text_color(muted)
                            .child(kind.clone()),
                    )
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .text_size(px(design::CHROME_SIZE))
                                    .font_semibold()
                                    .child(self.title.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(design::SECONDARY_SIZE))
                                    .text_color(muted)
                                    .child(if self.markdown {
                                        language.text(Message::EditorMarkdownDocument).to_owned()
                                    } else {
                                        kind
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .mt(px(12.0))
                    .text_size(px(design::SECONDARY_SIZE))
                    .line_height(gpui::relative(1.7))
                    .text_color(muted)
                    .child(self.source.display()),
            )
            .child(section(Message::EditorStatistics, statistics))
            .child(section(Message::EditorProperties, properties))
            .child(actions);
        v_flex()
            .id("file-info-panel")
            .relative()
            .w(px(design::clamp_details_width(self.details_width)))
            .min_w(px(design::DETAILS_MIN_WIDTH))
            .max_w(gpui::relative(0.42))
            .h_full()
            .flex_shrink_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .child(self.render_details_header(cx))
            .child(body)
            .child(self.render_details_resize_handle(cx))
            .into_any_element()
    }
}
