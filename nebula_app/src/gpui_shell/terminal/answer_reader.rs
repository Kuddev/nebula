use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;

use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, KeyDownEvent, ObjectFit, ParentElement as _, Render,
    RenderImage, SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _,
    WeakEntity, Window, div, img, px,
};
use gpui_component::text::{
    MarkdownExtensions, MarkdownNode, TextView, TextViewState, TextViewStyle, markdown_ast,
};

use crate::assistant_answer::AnswerSnapshot;
use crate::assistant_answer::document::{self, AnswerImage, IMAGE_LANGUAGE, ReaderDocument};
use crate::gpui_shell::prelude::*;

const IMAGE_BUDGET: usize = 64 * 1024 * 1024;
const LITERAL_BLOCK: &str = "nebula-answer-literal";

pub(super) enum ReaderEvent {
    Close,
}

enum ImageStatus {
    Waiting,
    Loading,
    Ready(Arc<RenderImage>),
    Failed(String),
}

struct ReaderImage {
    spec: AnswerImage,
    selected_path: Option<PathBuf>,
    status: ImageStatus,
    revision: u64,
    bytes: usize,
}

pub(super) struct AnswerReader {
    pub focus_handle: FocusHandle,
    snapshot: AnswerSnapshot,
    text: Entity<TextViewState>,
    raw_text: Entity<TextViewState>,
    raw_mode: bool,
    raw_initialized: bool,
    pending_markdown: Option<String>,
    extensions: MarkdownExtensions,
    notice: Option<String>,
    images: Vec<ReaderImage>,
    image_queue: VecDeque<usize>,
    decoding: bool,
    image_bytes: usize,
    preview: Option<Arc<RenderImage>>,
    preparing: bool,
    attention: bool,
    newer_answer: bool,
}

impl EventEmitter<ReaderEvent> for AnswerReader {}

impl AnswerReader {
    pub fn new(snapshot: AnswerSnapshot, cx: &mut Context<Self>) -> Self {
        let source = snapshot.content.source().cloned();
        let notice = snapshot.content.notice();
        let reader = Self {
            focus_handle: cx.focus_handle(),
            snapshot,
            text: cx.new(|cx| TextViewState::markdown("", cx)),
            raw_text: cx.new(|cx| TextViewState::markdown("", cx)),
            raw_mode: false,
            raw_initialized: false,
            pending_markdown: None,
            extensions: MarkdownExtensions::default(),
            notice,
            images: Vec::new(),
            image_queue: VecDeque::new(),
            decoding: false,
            image_bytes: 0,
            preview: None,
            preparing: source.is_some(),
            attention: false,
            newer_answer: false,
        };
        if let Some(source) = source {
            cx.spawn(async move |reader, cx| {
                let document = cx.background_spawn(async move { document::prepare(&source) }).await;
                let _ = reader.update(cx, |reader, cx| reader.prepared(document, cx));
            })
            .detach();
        }
        reader
    }

    pub fn needs_attention(&mut self, cx: &mut Context<Self>) {
        self.attention = true;
        cx.notify();
    }

    pub fn answer_arrived(&mut self, cx: &mut Context<Self>) {
        self.newer_answer = true;
        cx.notify();
    }

    fn prepared(&mut self, document: ReaderDocument, cx: &mut Context<Self>) {
        let starts =
            document.images.iter().map(|image| image.placeholder_start).collect::<Vec<_>>();
        self.images = document
            .images
            .into_iter()
            .map(|spec| ReaderImage {
                spec,
                selected_path: None,
                status: ImageStatus::Waiting,
                revision: 0,
                bytes: 0,
            })
            .collect();
        if document.images_omitted > 0 {
            self.notice = Some(format!(
                "另有 {} 张图片超过本次阅读上限，原文未删减。",
                document.images_omitted
            ));
        }
        let weak = cx.entity().downgrade();
        self.extensions = MarkdownExtensions::default()
            .block_parser(move |node, context| {
                if contains_uncontrolled_media(node) {
                    let source = context.node_source(node)?.to_owned();
                    return Some(
                        MarkdownNode::new(LITERAL_BLOCK, ()).text(source.clone()).markdown(source),
                    );
                }
                let index = image_placeholder_index(node, context.offset(), &starts)?;
                let source = context.node_source(node)?.to_owned();
                Some(MarkdownNode::new(IMAGE_LANGUAGE, index).text("[图片]").markdown(source))
            })
            .block_renderer(IMAGE_LANGUAGE, move |node, _, cx| match node.data::<usize>() {
                Some(index) => render_image(&weak, *index, cx),
                None => div().into_any_element(),
            })
            .block_renderer(LITERAL_BLOCK, |node, _, _| div().child(node.as_text().to_owned()));
        self.pending_markdown = Some(document.markdown);
        self.preparing = false;
        self.image_queue.extend(0..self.images.len());
        self.decode_next(cx);
        cx.notify();
    }

    fn decode_next(&mut self, cx: &mut Context<Self>) {
        if self.decoding {
            return;
        }
        let Some(index) = self.image_queue.pop_front() else { return };
        let image = &mut self.images[index];
        image.status = ImageStatus::Loading;
        let revision = image.revision;
        let selected_path = image.selected_path.clone();
        let target = image.spec.target.clone();
        let base = self.snapshot.cwd.clone();
        self.decoding = true;
        cx.spawn(async move |reader, cx| {
            let result = cx
                .background_spawn(async move {
                    let path = match selected_path {
                        Some(path) => path,
                        None => document::local_image_path(
                            &target,
                            base.as_deref().ok_or("回答未携带本地目录，请主动选择图片文件。")?,
                        )?,
                    };
                    let bytes = document::read_image(&path)?;
                    super::inline_image::decode_bytes(&bytes)
                })
                .await;
            let _ = reader.update(cx, |reader, cx| {
                reader.decoding = false;
                if reader.images[index].revision == revision {
                    match result {
                        Ok((image, bytes))
                            if reader.image_bytes.saturating_add(bytes) <= IMAGE_BUDGET =>
                        {
                            reader.image_bytes += bytes;
                            reader.images[index].bytes = bytes;
                            reader.images[index].status = ImageStatus::Ready(image);
                        },
                        Ok(_) => {
                            reader.images[index].status = ImageStatus::Failed(
                                "图片超过本次阅读的 64 MiB 总预算，未显示。".into(),
                            )
                        },
                        Err(error) => reader.images[index].status = ImageStatus::Failed(error),
                    }
                }
                reader.decode_next(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn choose_image(&mut self, index: usize, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择 PNG / JPEG 图片".into()),
        });
        cx.spawn(async move |reader, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = reader.update(cx, |reader, cx| {
                let image = &mut reader.images[index];
                reader.image_bytes = reader.image_bytes.saturating_sub(image.bytes);
                image.bytes = 0;
                image.revision += 1;
                image.selected_path = Some(path);
                image.status = ImageStatus::Waiting;
                reader.image_queue.retain(|queued| *queued != index);
                reader.image_queue.push_back(index);
                reader.decode_next(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            if self.preview.take().is_none() {
                cx.emit(ReaderEvent::Close);
            }
            cx.notify();
            cx.stop_propagation();
        } else if event.keystroke.modifiers.control
            && event.keystroke.modifiers.shift
            && event.keystroke.key == "c"
        {
            let text = if self.raw_mode { &self.raw_text } else { &self.text };
            let selected = text.read(cx).selected_text();
            if !selected.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(selected));
            }
            cx.stop_propagation();
        }
    }
}

fn contains_uncontrolled_media(node: &markdown_ast::Node) -> bool {
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if matches!(
            node,
            markdown_ast::Node::Image(_)
                | markdown_ast::Node::ImageReference(_)
                | markdown_ast::Node::Html(_)
        ) {
            return true;
        }
        if let Some(children) = node.children() {
            pending.extend(children);
        }
    }
    false
}

fn image_placeholder_index(
    node: &markdown_ast::Node,
    offset: usize,
    starts: &[usize],
) -> Option<usize> {
    let markdown_ast::Node::Code(code) = node else { return None };
    if code.lang.as_deref() != Some(IMAGE_LANGUAGE) {
        return None;
    }
    let index: usize = code.value.parse().ok()?;
    let position = node.position()?;
    (starts.get(index).copied()? == offset.checked_add(position.start.offset)?).then_some(index)
}

fn render_image(reader: &WeakEntity<AnswerReader>, index: usize, cx: &mut App) -> AnyElement {
    let Some(entity) = reader.upgrade() else { return div().into_any_element() };
    let state = entity.read(cx);
    let Some(image) = state.images.get(index) else { return div().into_any_element() };
    let target: SharedString = image.spec.target.clone().into();
    let alt: SharedString = image.spec.alt.clone().into();
    let mut block = v_flex().w_full().gap_2().py_2();
    match &image.status {
        ImageStatus::Ready(image) => {
            let image = image.clone();
            let enlarged = image.clone();
            let weak = reader.clone();
            block = block
                .child(
                    div()
                        .id(("answer-image", index))
                        .w_full()
                        .h(px(320.0))
                        .cursor_pointer()
                        .child(img(image).size_full().object_fit(ObjectFit::Contain))
                        .on_click(move |_, _, cx| {
                            let _ = weak.update(cx, |reader, cx| {
                                reader.preview = Some(enlarged.clone());
                                cx.notify();
                            });
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("点击放大 · Esc 返回"),
                );
        },
        ImageStatus::Waiting | ImageStatus::Loading => {
            block = block.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("正在检查并加载本地图片…"),
            );
        },
        ImageStatus::Failed(error) => {
            let weak = reader.clone();
            block = block
                .child(div().text_sm().text_color(cx.theme().muted_foreground).child(error.clone()))
                .child(
                    Button::new(("answer-pick-image", index))
                        .label("选择本地图片")
                        .ghost()
                        .small()
                        .on_click(move |_, _, cx| {
                            let _ = weak.update(cx, |reader, cx| reader.choose_image(index, cx));
                        }),
                );
        },
    }
    block
        .child(div().text_xs().text_color(cx.theme().muted_foreground).child(alt))
        .child(div().text_xs().text_color(cx.theme().muted_foreground).child(target))
        .into_any_element()
}

impl Render for AnswerReader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(markdown) = self.pending_markdown.take() {
            cx.on_next_frame(window, move |reader, _, cx| {
                reader.text.update(cx, |text, cx| text.push_str(&markdown, cx));
            });
        }
        if self.raw_mode && !self.raw_initialized {
            self.raw_initialized = true;
            if let Some(source) = self.snapshot.content.source() {
                let markdown = document::literal_markdown(source);
                self.raw_text.update(cx, |text, cx| text.push_str(&markdown, cx));
            }
        }
        let provider = match self.snapshot.provider.as_str() {
            "claude" => "Claude Code",
            "codex" => "Codex",
            _ => "Agent",
        };
        let muted = cx.theme().muted_foreground;
        let has_source = self.snapshot.content.source().is_some();
        let text = if self.raw_mode { &self.raw_text } else { &self.text };
        let extensions =
            if self.raw_mode { MarkdownExtensions::default() } else { self.extensions.clone() };
        let mut root = v_flex()
            .size_full()
            .min_w_0()
            .relative()
            .overflow_hidden()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(
                h_flex()
                    .h(px(32.0))
                    .flex_shrink_0()
                    .px_2()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("reader-return")
                            .label("终端")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ReaderEvent::Close))),
                    )
                    .child(div().text_sm().flex_1().child(format!("{provider} · 最近完整回答")))
                    .child(
                        Button::new("reader-source")
                            .label(if self.raw_mode { "阅读" } else { "原文" })
                            .ghost()
                            .small()
                            .disabled(!has_source)
                            .on_click(cx.listener(|reader, _, _, cx| {
                                reader.raw_mode = !reader.raw_mode;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("reader-copy")
                            .label("复制原文")
                            .ghost()
                            .small()
                            .disabled(!has_source)
                            .on_click(cx.listener(|reader, _, _, cx| {
                                if let Some(source) = reader.snapshot.content.source() {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        source.to_string(),
                                    ));
                                }
                            })),
                    ),
            )
            .when(self.attention, |root| {
                root.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_sm()
                        .text_color(cx.theme().warning)
                        .child("终端需要你处理输入或审批；点击「终端」返回。"),
                )
            })
            .when(self.newer_answer, |root| {
                root.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_sm()
                        .text_color(muted)
                        .child("已收到新回答；本页不跳动，返回终端再点「阅读」查看。"),
                )
            })
            .when_some(self.notice.clone(), |root, notice| {
                root.child(div().px_3().py_2().text_sm().text_color(muted).child(notice))
            })
            .when(self.preparing, |root| {
                root.child(div().p_3().text_sm().text_color(muted).child("正在整理完整回答…"))
            })
            .child(
                div().flex_1().min_h_0().min_w_0().px_3().py_2().child(
                    TextView::new(text)
                        .selectable(true)
                        .scrollable(true)
                        .markdown_extensions(extensions)
                        .on_link_click(|url, _, _, cx| {
                            if url.starts_with("https://") || url.starts_with("http://") {
                                cx.open_url(url);
                            }
                        })
                        .style(TextViewStyle {
                            highlight_theme: cx.theme().highlight_theme.clone(),
                            is_dark: cx.theme().is_dark(),
                            ..TextViewStyle::default()
                        }),
                ),
            );
        if let Some(image) = self.preview.clone() {
            root = root.child(
                v_flex()
                    .absolute()
                    .inset_0()
                    .occlude()
                    .bg(cx.theme().background)
                    .p_3()
                    .gap_2()
                    .child(
                        h_flex().justify_between().child("图片预览").child(
                            Button::new("reader-close-preview")
                                .label("返回阅读 · Esc")
                                .ghost()
                                .small()
                                .on_click(cx.listener(|reader, _, _, cx| {
                                    reader.preview = None;
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .child(img(image).size_full().object_fit(ObjectFit::Contain)),
                    ),
            );
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_generated_image_placeholders_can_load_files() {
        let document = document::prepare("```nebula-answer-image\n0\n```\n\n![图](plot.png)");
        let starts =
            document.images.iter().map(|image| image.placeholder_start).collect::<Vec<_>>();
        let root = markdown::to_mdast(&document.markdown, &markdown::ParseOptions::gfm()).unwrap();
        let nodes = root.children().unwrap();
        assert_eq!(image_placeholder_index(&nodes[0], 0, &starts), None);
        assert!(nodes.iter().any(|node| image_placeholder_index(node, 0, &starts) == Some(0)));
        assert!(!contains_uncontrolled_media(&root));
    }

    #[test]
    fn uncontrolled_media_falls_back_before_component_can_fetch_it() {
        for source in [
            "![x](https://example.invalid/a)",
            "<img src=\"https://example.invalid/a\">",
            "|图|\n|-|\n|![x](file:///private.png)|",
        ] {
            let root = markdown::to_mdast(source, &markdown::ParseOptions::gfm()).unwrap();
            assert!(contains_uncontrolled_media(&root));
        }
    }
}
