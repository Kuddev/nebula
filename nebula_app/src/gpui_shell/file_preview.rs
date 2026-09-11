//! A file tooltip owns its background request; closing it discards the result.

use crate::i18n::Message;
use gpui::prelude::*;
use gpui::{Context, Image, ImageFormat, IntoElement, ObjectFit, Render, Window, div, img, px};
use gpui_component::ActiveTheme;
use std::{path::PathBuf, sync::Arc};

pub(super) struct FilePreview {
    path: PathBuf,
    image: Option<Arc<Image>>,
    info: Option<String>,
    failed: bool,
}

impl FilePreview {
    pub(super) fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let source = path.clone();
        let task = cx
            .background_executor()
            .spawn(async move { crate::platform::file_preview::load(&source) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(preview) => {
                        view.image = Some(Arc::new(Image::from_bytes(
                            ImageFormat::Png,
                            preview.png.to_vec(),
                        )));
                        view.info = Some(match preview.pages {
                            Some(pages) => super::config::ui_language(cx).format(
                                Message::FilesPreviewPages,
                                &[("pages", &pages.to_string())],
                            ),
                            None => format!("{} × {}", preview.width, preview.height),
                        });
                    },
                    Err(error) => {
                        log::debug!("file preview unavailable: {error}");
                        view.failed = true;
                    },
                }
                cx.notify();
            });
        })
        .detach();
        Self { path, image: None, info: None, failed: false }
    }
}

impl Render for FilePreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let language = super::config::ui_language(cx);
        div()
            .w(px(300.0))
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .shadow_md()
            .child(
                div().text_xs().truncate().child(
                    self.path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                ),
            )
            .when_some(self.image.clone(), |root, image| {
                root.child(img(image).w_full().h(px(220.0)).object_fit(ObjectFit::Contain))
            })
            .when(self.image.is_none(), |root| {
                root.child(div().h(px(60.0)).text_xs().child(language.text(if self.failed {
                    Message::FilesPreviewUnavailable
                } else {
                    Message::FilesPreviewLoading
                })))
            })
            .when_some(self.info.clone(), |root, info| {
                root.child(div().text_xs().text_color(cx.theme().muted_foreground).child(info))
            })
    }
}
