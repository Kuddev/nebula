//! Reading layout and per-block presentation choices for the Markdown preview.

use super::*;
use crate::i18n::Message;
use gpui::prelude::*;
use gpui::{IntoElement, ObjectFit, SharedString, Window, div, img, px};
use gpui_component::{
    ActiveTheme, ElementExt as _, WindowExt as _,
    text::{MarkdownExtensions, MarkdownNode, TextView, TextViewStyle},
};
use std::{
    path::PathBuf,
    sync::{Arc, LazyLock},
    time::Duration,
};

const SELECTION_SCROLL_INTERVAL: Duration = Duration::from_millis(16);

fn selection_scroll_delta(
    has_selection: bool,
    pointer_y: Pixels,
    bounds: Bounds<Pixels>,
) -> Option<Pixels> {
    if !has_selection || bounds.size.height <= px(0.0) {
        return None;
    }
    gpui_component::scroll::AutoScroll::compute_delta(pointer_y, bounds)
}

#[derive(Clone, Debug)]
struct CenteredHtml {
    source: String,
    images: Option<Vec<HtmlImage>>,
}
#[derive(Clone, Debug)]
struct HtmlImage {
    source: String,
    link: Option<String>,
    width: Option<f32>,
    height: Option<f32>,
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    static ATTRIBUTES: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"(?is)\b([\w-]+)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))"#).unwrap()
    });
    ATTRIBUTES.captures_iter(tag).find_map(|capture| {
        if !capture[1].eq_ignore_ascii_case(name) {
            return None;
        }
        let value = capture.get(2).or_else(|| capture.get(3)).or_else(|| capture.get(4))?;
        Some(html_escape::decode_html_entities(value.as_str()).into_owned())
    })
}

fn centered_html(source: &str) -> Option<CenteredHtml> {
    static OUTER: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?is)^\s*<(p|div|h[1-6])\b([^>]*)>(.*)</([a-z0-9]+)>\s*$").unwrap()
    });
    static TAGS: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?is)<!--.*?-->|<[^>]*>").unwrap());
    static IMAGE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?is)<img\b[^>]*>").unwrap());
    let outer = OUTER.captures(source)?;
    if !outer[1].eq_ignore_ascii_case(&outer[4]) {
        return None;
    }
    let centered = attribute(&outer[2], "align")
        .is_some_and(|value| value.eq_ignore_ascii_case("center"))
        || attribute(&outer[2], "style").is_some_and(|style| {
            style.split(';').any(|property| {
                property.split_once(':').is_some_and(|(name, value)| {
                    name.trim().eq_ignore_ascii_case("text-align")
                        && value.trim().eq_ignore_ascii_case("center")
                })
            })
        });
    if !centered {
        return None;
    }
    let body = &outer[3];
    let images = if TAGS.replace_all(body, "").trim().is_empty() && IMAGE.is_match(body) {
        let mut images = Vec::new();
        for image in IMAGE.find_iter(body) {
            let tag = image.as_str();
            let Some(source) = attribute(tag, "src") else { return None };
            let prefix = &body[..image.start()];
            let link = prefix
                .rfind("<a ")
                .filter(|start| prefix.rfind("</a>").is_none_or(|end| *start > end))
                .and_then(|start| attribute(&prefix[start..], "href"));
            let dimension = |name| {
                attribute(tag, name)
                    .and_then(|value| value.parse::<f32>().ok())
                    .filter(|value| value.is_finite() && *value > 0.0)
            };
            images.push(HtmlImage {
                source,
                link,
                width: dimension("width"),
                height: dimension("height"),
            });
        }
        Some(images)
    } else {
        None
    };
    Some(CenteredHtml { source: source.to_owned(), images })
}

pub(super) fn extensions(base: Option<PathBuf>) -> MarkdownExtensions {
    super::super::molecule_view::markdown_extensions()
        .block_parser(|node, _| {
            let markdown::mdast::Node::Html(html) = node else { return None };
            let centered = centered_html(&html.value)?;
            Some(
                MarkdownNode::new("pebrel-centered-html", centered)
                    .text(html.value.clone())
                    .markdown(html.value.clone()),
            )
        })
        .block_renderer("pebrel-centered-html", move |node, _, cx| {
            let data = node.data::<CenteredHtml>().unwrap();
            if let Some(images) = &data.images {
                let mut row =
                    h_flex().w_full().min_w_0().flex_wrap().justify_center().items_center().gap_1();
                for (index, image) in images.iter().enumerate() {
                    let source: gpui::ImageSource = if image.source.starts_with("https://")
                        || image.source.starts_with("http://")
                        || image.source.starts_with("data:")
                    {
                        image.source.clone().into()
                    } else {
                        base.as_ref()
                            .map(|base| base.join(&image.source))
                            .unwrap_or_else(|| PathBuf::from(&image.source))
                            .into()
                    };
                    row = row.child(
                        img(source)
                            .id(("centered-image", index))
                            .max_w_full()
                            .object_fit(ObjectFit::Contain)
                            .when_some(image.width, |image, width| image.w(px(width)))
                            .when_some(image.height, |image, height| image.h(px(height)))
                            .when_some(image.link.clone(), |image, link| {
                                image.cursor_pointer().on_click(move |_, _, cx| cx.open_url(&link))
                            }),
                    );
                }
                row.into_any_element()
            } else {
                div()
                    .w_full()
                    .min_w_0()
                    .max_w_full()
                    .text_center()
                    .whitespace_normal()
                    .child(
                        TextView::html("centered-text", data.source.clone())
                            .w_full()
                            .min_w_0()
                            .selectable(true)
                            .style(TextViewStyle {
                                image_base: base.clone().map(Arc::from),
                                highlight_theme: cx.theme().highlight_theme.clone(),
                                is_dark: cx.theme().is_dark(),
                                ..Default::default()
                            }),
                    )
                    .into_any_element()
            }
        })
}

/// Change only the fence's language token. Code, indentation, fence lengths,
/// trailing metadata and every other block remain byte-for-byte intact.
fn fence_language(source: &str, start: usize, end: usize, language: &str) -> Option<String> {
    if language.chars().any(|ch| ch.is_whitespace() || matches!(ch, '`' | '~')) {
        return None;
    }
    let block = source.get(start..end)?;
    let line = block.split('\n').next()?;
    let trimmed = line.trim_start();
    let marker = trimmed.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let count = trimmed.chars().take_while(|ch| *ch == marker).count();
    if count < 3 {
        return None;
    }
    let info = start + (line.len() - trimmed.len()) + count;
    let old = &source[info..start + line.len()];
    let token_start = info + old.len() - old.trim_start().len();
    let token_end = token_start
        + source[token_start..start + line.len()]
            .find(char::is_whitespace)
            .unwrap_or(start + line.len() - token_start);
    let language = if language.is_empty() { "text" } else { language };
    Some(format!("{}{}{}", &source[..token_start], language, &source[token_end..]))
}

impl TextFileView {
    fn start_preview_selection_scroll(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.preview_selection_scroll_active {
            return;
        }
        self.preview_selection_scroll_active = true;
        self.preview_selection_scroll_epoch = self.preview_selection_scroll_epoch.wrapping_add(1);
        let epoch = self.preview_selection_scroll_epoch;
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            let mut refresh_selection = false;
            loop {
                executor.timer(SELECTION_SCROLL_INTERVAL).await;
                let tick = this.update_in(cx, |view, window, cx| {
                    view.preview_selection_scroll_tick(epoch, window, cx)
                });
                let Ok(Some(scrolled)) = tick else { break };

                // The list reflows after the previous tick. Re-dispatching the
                // current pointer position makes the window-level selection
                // endpoint follow the newly visible Markdown block.
                if refresh_selection {
                    let _ = cx.update(|window, cx| {
                        let _ = window.dispatch_event(
                            gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                                position: window.mouse_position(),
                                pressed_button: Some(MouseButton::Left),
                                modifiers: window.modifiers(),
                            }),
                            cx,
                        );
                    });
                }
                refresh_selection = scrolled;
            }
        })
        .detach();
    }

    pub(super) fn stop_preview_selection_scroll(&mut self) {
        if !self.preview_selection_scroll_active {
            return;
        }
        self.preview_selection_scroll_active = false;
        self.preview_selection_scroll_epoch = self.preview_selection_scroll_epoch.wrapping_add(1);
    }

    fn preview_selection_scroll_tick(
        &mut self,
        epoch: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        if epoch != self.preview_selection_scroll_epoch || !self.preview_selection_scroll_active {
            return None;
        }
        let bounds = *self.preview_bounds.borrow();
        let Some(delta) = selection_scroll_delta(
            window.has_text_selection(cx),
            window.mouse_position().y,
            bounds,
        ) else {
            return Some(false);
        };
        let before = self.scroll.scroll_px_offset_for_scrollbar();
        self.scroll.scroll_by(delta);
        let scrolled = self.scroll.scroll_px_offset_for_scrollbar() != before;
        if scrolled {
            cx.notify();
        }
        Some(scrolled)
    }

    pub(super) fn render_markdown_preview(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let blocks = self.blocks.clone();
        let owner = cx.entity().downgrade();
        let extensions = self.preview_extensions.clone();
        let scroll = self.scroll.clone();
        let bounds = self.preview_bounds.clone();
        let style = TextViewStyle {
            image_base: self.path.parent().map(Arc::from),
            highlight_theme: cx.theme().highlight_theme.clone(),
            is_dark: cx.theme().is_dark(),
            paragraph_gap: gpui::rems(0.7),
            heading_base_font_size: px(reader_presentation::HEADING_BASE),
            code_block: {
                let mut style = gpui::StyleRefinement::default();
                style.padding.top = Some(px(16.0).into());
                style.background = Some(super::super::theme::code_block_background(cx).into());
                style
            },
            ..Default::default()
        };
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .relative()
            .overflow_hidden()
            .px(px(reader_presentation::PAGE_MARGIN))
            .pt(px(reader_presentation::TOP_MARGIN))
            .debug_selector(|| "markdown-preview-viewport".to_owned())
            .on_prepaint(move |viewport, _, _| *bounds.borrow_mut() = viewport)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.start_preview_selection_scroll(window, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.stop_preview_selection_scroll();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.stop_preview_selection_scroll();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|_, event: &gpui::MouseDownEvent, window, cx| {
                    let text = window.selected_text(cx).to_string();
                    if !text.trim().is_empty() {
                        cx.emit(TextFileEvent::SelectionContextMenuRequested {
                            position: event.position,
                            text,
                        });
                        cx.stop_propagation();
                    }
                }),
            )
            .child(
                gpui::list(scroll.clone(), move |index, window, cx| {
                    let cached = blocks.borrow().get(index).cloned().flatten();
                    let block = cached.unwrap_or_else(|| {
                        let (source, selected) = owner
                            .upgrade()
                            .map(|owner| {
                                let owner = owner.read(cx);
                                (owner.outline.block_source(index), owner.all_selected)
                            })
                            .unwrap_or_default();
                        let state = cx.new(|cx| TextViewState::markdown(&source, cx));
                        if selected {
                            state.update(cx, |state, cx| state.select_all(cx));
                        }
                        blocks.borrow_mut()[index] = Some(state.clone());
                        state
                    });
                    let owner = owner.clone();
                    let block_extensions = window
                        .use_keyed_state(("reader-code-extensions", index), cx, |_, _| {
                            super::code_actions::extensions(
                                extensions.clone(),
                                owner.clone(),
                                index,
                            )
                        })
                        .read(cx)
                        .clone();
                    div()
                        .w_full()
                        .min_w_0()
                        .py_1()
                        .child(
                            div()
                                .w_full()
                                .max_w(px(reader_presentation::PAGE_WIDTH))
                                .mx_auto()
                                .min_w_0()
                                .debug_selector(move || format!("markdown-preview-block-{index}"))
                                .text_size(px(reader_presentation::BODY_SIZE))
                                .line_height(gpui::relative(reader_presentation::LINE_HEIGHT))
                                .whitespace_normal()
                                .child(
                                    TextView::new(&block)
                                        .w_full()
                                        .min_w_0()
                                        .max_w_full()
                                        .selectable(true)
                                        .scrollable(false)
                                        .style(style.clone())
                                        .markdown_extensions(block_extensions),
                                ),
                        )
                        .into_any_element()
                })
                .size_full(),
            )
            .child(
                div()
                    .id("markdown-preview-scrollbar-host")
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(16.0))
                    .debug_selector(|| "markdown-preview-scrollbar".to_owned())
                    .on_hover(cx.listener(|view, hovered: &bool, _, cx| {
                        view.preview_scrollbar_hovered = *hovered;
                        cx.notify();
                    }))
                    .child(gpui_component::scroll::Scrollbar::vertical(&scroll).scrollbar_show(
                        if self.preview_scrollbar_hovered {
                            gpui_component::scroll::ScrollbarShow::Hover
                        } else {
                            gpui_component::scroll::ScrollbarShow::Scrolling
                        },
                    )),
            )
            .into_any_element()
    }

    pub(super) fn set_preview_language(
        &mut self,
        block: usize,
        start: usize,
        end: usize,
        language: &str,
        cx: &mut Context<Self>,
    ) {
        let Some((start, end)) = self.outline.source_span(block, start, end) else { return };
        let Some(source) = self.outline.blocks.get(block) else { return };
        let Some(next) = fence_language(source, start, end, language) else { return };
        self.outline.replace_block(block, next, end);
        let source = self.outline.block_source(block);
        if let Some(state) = self.blocks.borrow().get(block).cloned().flatten() {
            state.update(cx, |state, cx| state.set_text(&source, cx));
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_auto_scroll_requires_a_selection_near_a_viewport_edge() {
        let bounds = Bounds::new(gpui::point(px(0.0), px(100.0)), gpui::size(px(600.0), px(300.0)));
        assert!(selection_scroll_delta(false, px(399.0), bounds).is_none());
        assert!(selection_scroll_delta(true, px(250.0), bounds).is_none());
        assert!(
            selection_scroll_delta(true, px(101.0), bounds).is_some_and(|delta| delta < px(0.0))
        );
        assert!(
            selection_scroll_delta(true, px(399.0), bounds).is_some_and(|delta| delta > px(0.0))
        );
    }

    #[test]
    fn readme_centered_badges_keep_links_dimensions_and_entities() {
        let source = r#"<p ALIGN="center"><img src="one.png?a=1&amp;b=2" width="148" height="148"><a href="https://example.com"><img src="two.svg"></a></p>"#;
        let parsed = centered_html(source).unwrap();
        let images = parsed.images.unwrap();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].source, "one.png?a=1&b=2");
        assert_eq!(images[0].width, Some(148.0));
        assert_eq!(images[1].link.as_deref(), Some("https://example.com"));
        assert!(centered_html("<p>ordinary</p>").is_none());
        assert!(
            centered_html("<div style='text-align: center'><b>Centered</b></div>")
                .unwrap()
                .images
                .is_none()
        );
    }
    #[test]
    fn language_switch_changes_one_fence_without_rewriting_code() {
        let source =
            "before\n\n```wrong title=demo\nprint('你好')\n```\n\n```js\nconst x = 1;\n```";
        let start = source.find("```wrong").unwrap();
        let end = source.find("\n\n```js").unwrap();
        let changed = fence_language(source, start, end, "python").unwrap();
        assert_eq!(changed, source.replacen("```wrong", "```python", 1));
        assert!(fence_language(source, start, end, "bad\nlang").is_none());
        assert_eq!(fence_language("~~~\nx\n~~~", 0, 9, "rust").unwrap(), "~~~rust\nx\n~~~");
    }
}
