use super::*;

impl TextFileView {
    pub(super) fn render_info(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let language = super::super::config::ui_language(cx);
        let muted = cx.theme().muted_foreground;
        let text = self.input.read(cx).value();
        let mut rows = vec![
            (
                Message::EditorType,
                self.path.extension().unwrap_or_default().to_string_lossy().to_uppercase(),
            ),
            (Message::EditorLineCount, text.lines().count().to_string()),
            (Message::EditorCharacters, text.chars().count().to_string()),
            (
                Message::EditorStatus,
                language
                    .text(if self.dirty { Message::EditorUnsaved } else { Message::EditorSaved })
                    .to_owned(),
            ),
        ];
        if let Some(document) = &self.document {
            rows.insert(1, (Message::EditorSize, format!("{} B", document.bytes.len())));
            if let Some(modified) = document.modified {
                let date: chrono::DateTime<chrono::Local> = modified.into();
                rows.insert(
                    2,
                    (Message::EditorModified, date.format("%Y-%m-%d %H:%M").to_string()),
                );
            }
            rows.push((
                Message::EditorEncodingLabel,
                if document.invalid_encoding {
                    "—"
                } else if document.bom {
                    "UTF-8 BOM"
                } else {
                    "UTF-8"
                }
                .into(),
            ));
            rows.push((
                Message::EditorLineEnding,
                if document.crlf { "CRLF" } else { "LF" }.into(),
            ));
        }
        let path = self.path.clone();
        let name = self.title.clone();
        let reveal = self.path.clone();
        let external = self.path.clone();
        v_flex()
            .id("file-info-panel")
            .w(px(250.0))
            .max_w(gpui::relative(0.38))
            .h_full()
            .flex_shrink_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .p_3()
            .gap_3()
            .overflow_y_scroll()
            .child(div().text_xs().font_semibold().child(language.text(Message::EditorInfo)))
            .child(div().text_sm().font_semibold().child(self.title.clone()))
            .child(div().text_xs().text_color(muted).child(self.path.display().to_string()))
            .children(rows.into_iter().map(|(label, value)| {
                h_flex()
                    .w_full()
                    .items_start()
                    .gap_2()
                    .text_xs()
                    .child(div().flex_shrink_0().text_color(muted).child(language.text(label)))
                    .child(div().flex_1().min_w_0().text_right().child(value))
            }))
            .child(
                Button::new("info-copy-path")
                    .ghost()
                    .small()
                    .justify_start()
                    .label(language.text(Message::EditorCopyPath))
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                            path.display().to_string(),
                        ))
                    }),
            )
            .child(
                Button::new("info-copy-name")
                    .ghost()
                    .small()
                    .justify_start()
                    .label(language.text(Message::EditorCopyName))
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(name.clone()))
                    }),
            )
            .child(
                Button::new("info-reveal")
                    .ghost()
                    .small()
                    .justify_start()
                    .label(language.text(Message::EditorReveal))
                    .on_click(move |_, _, _| {
                        super::super::workspace::reveal_in_file_manager(&reveal)
                    }),
            )
            .child(
                Button::new("info-open")
                    .ghost()
                    .small()
                    .justify_start()
                    .label(language.text(Message::EditorOpenExternal))
                    .on_click(move |_, _, _| {
                        super::super::workspace::open_in_file_manager(&external)
                    }),
            )
            .into_any_element()
    }
}
