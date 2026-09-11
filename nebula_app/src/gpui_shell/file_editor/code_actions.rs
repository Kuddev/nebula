//! Visible-block-scoped language picker and code clipboard actions.
use super::*;
use gpui::{ClickEvent, ClipboardItem, FocusHandle, Hsla, RenderOnce, Window, anchored, deferred};
use gpui_component::Size;
use gpui_component::list::{List, ListDelegate, ListEvent, ListState};
use gpui_component::text::{MarkdownExtensions, MarkdownNode, TextView, TextViewStyle};

use super::super::copy_feedback::CopyFeedback;

#[derive(Clone)]
pub(super) struct CodeSpec {
    pub source: SharedString,
    pub language: Option<SharedString>,
    pub span: Option<(usize, usize)>,
}

struct CodeLanguage {
    list: Entity<ListState<LanguageListDelegate>>,
    focus: FocusHandle,
    open: bool,
    plain: SharedString,
    selected: SharedString,
    copy_feedback: Entity<CopyFeedback>,
    _copy_feedback_subscription: Subscription,
    span: Option<(usize, usize)>,
    _subscription: Subscription,
    content: Entity<TextViewState>,
    markdown: String,
}

/// Colors used by the approved code-block prototype. The light
/// values are the Paper prototype's actual roles; the dark values are its
/// Nord companion roles. Keeping these roles together prevents the code block
/// from falling back to a generic component card when the surrounding theme
/// changes.
#[derive(Clone, Copy)]
struct CodeUiColors {
    ink: Hsla,
    secondary: Hsla,
    code: Hsla,
    hover: Hsla,
    popup: Hsla,
    line: Hsla,
    accent: Hsla,
}

fn code_ui_colors(cx: &App) -> CodeUiColors {
    let theme = cx.theme();
    CodeUiColors {
        ink: super::super::theme::code_block_foreground(cx),
        secondary: theme.muted_foreground,
        code: super::super::theme::code_block_background(cx),
        hover: theme.list_hover,
        popup: theme.popover,
        line: theme.border,
        accent: theme.ring,
    }
}

#[derive(Clone)]
struct LanguageOption {
    name: SharedString,
    glyph: SharedString,
    aliases: SharedString,
}

impl LanguageOption {
    fn new(name: SharedString) -> Self {
        let lower = name.to_lowercase();
        let (glyph, aliases) = match lower.as_str() {
            "rust" | "rs" => ("Rs", "rust rs"),
            "javascript" | "js" => ("JS", "javascript js"),
            "typescript" | "ts" => ("TS", "typescript ts"),
            "python" | "py" => ("Py", "python py"),
            "c" => ("C", "c"),
            "cpp" | "c++" => ("C+", "cpp c++"),
            "csharp" | "c#" => ("C#", "csharp c#"),
            "html" | "xml" => ("<> ", "html xml"),
            "css" => ("#{}", "css"),
            "json" => ("{}", "json"),
            "bash" | "shell" | "sh" | "zsh" => ("$", "bash shell sh zsh"),
            "markdown" | "md" => ("M↓", "markdown md"),
            "plaintext" | "text" | "纯文本" => ("≡", "plaintext text 纯文本"),
            _ => ("<> ", lower.as_str()),
        };
        Self { name, glyph: glyph.trim().into(), aliases: aliases.into() }
    }

    fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        query.is_empty()
            || self.name.to_lowercase().contains(&query)
            || self.aliases.to_lowercase().contains(&query)
    }
}

struct LanguageListDelegate {
    items: Vec<LanguageOption>,
    filtered: Vec<LanguageOption>,
    selected: SharedString,
}

impl LanguageListDelegate {
    fn new(items: Vec<LanguageOption>, selected: SharedString) -> Self {
        Self { filtered: items.clone(), items, selected }
    }

    fn set_selected(&mut self, selected: SharedString) {
        self.selected = selected;
    }
}

#[derive(Clone, IntoElement)]
struct LanguageRow {
    option: LanguageOption,
    highlighted: bool,
    checked: bool,
}

impl gpui_component::Selectable for LanguageRow {
    fn selected(mut self, selected: bool) -> Self {
        self.highlighted = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.highlighted
    }

    fn secondary_selected(self, _: bool) -> Self {
        self
    }
}

impl RenderOnce for LanguageRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = code_ui_colors(cx);
        let selector = format!("markdown-language-option-{}", self.option.name);
        h_flex()
            .debug_selector(move || selector.clone())
            .h(px(32.0))
            .w_full()
            .gap(px(9.0))
            .px(px(8.0))
            .rounded(px(3.0))
            .items_center()
            .text_size(px(12.0))
            .text_color(colors.ink)
            .when(self.highlighted, |row| row.bg(colors.hover))
            .when(!self.highlighted, |row| row.hover(|row| row.bg(colors.hover)))
            .child(
                div()
                    .w(px(18.0))
                    .h(px(18.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(11.0))
                    .text_color(colors.secondary)
                    .child(self.option.glyph),
            )
            // Keep the label as real text instead of an ellipsis-bearing
            // flex child. This matters for localized/CJK language names.
            .child(div().flex_none().child(self.option.name))
            .when(self.checked, |row| {
                row.child(Icon::new(IconName::Check).size(px(14.0)).text_color(colors.accent))
            })
    }
}

impl ListDelegate for LanguageListDelegate {
    type Item = LanguageRow;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.filtered.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        self.filtered.get(ix.row).cloned().map(|option| LanguageRow {
            checked: option.name == self.selected,
            highlighted: false,
            option,
        })
    }

    fn perform_search(
        &mut self,
        query: &str,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.filtered = self.items.iter().filter(|item| item.matches(query)).cloned().collect();
        Task::ready(())
    }

    fn set_selected_index(
        &mut self,
        _: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
    }

    fn confirm(&mut self, _: bool, _: &mut Window, _: &mut Context<ListState<Self>>) {}

    fn cancel(&mut self, _: &mut Window, _: &mut Context<ListState<Self>>) {}
}

fn standalone_markdown(code: &CodeSpec) -> String {
    let longest = code.source.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(3.max(longest + 1));
    format!("{fence}{}\n{}\n{fence}", code.language.as_deref().unwrap_or(""), code.source)
}

impl CodeLanguage {
    fn toggle(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.close(window, cx);
        } else {
            self.open = true;
            self.list.update(cx, |list, cx| list.focus(window, cx));
            cx.notify();
        }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        self.open = false;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn close_from_outside(
        &mut self,
        _: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close(window, cx);
    }
}

fn render_language_picker(
    state: Entity<CodeLanguage>,
    list: Entity<ListState<LanguageListDelegate>>,
    current: SharedString,
    open: bool,
    focus: FocusHandle,
    search_placeholder: &'static str,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let colors = code_ui_colors(cx);
    let picker = div()
        .id("markdown-language-picker")
        .debug_selector(|| "markdown-language-picker".to_owned())
        .relative()
        .w_auto()
        .min_w(px(48.0))
        .h(px(28.0))
        .child(
            div()
                .group("markdown-language-trigger")
                .id("markdown-language-trigger")
                .w_full()
                .h_full()
                .track_focus(&focus)
                .tab_stop(true)
                .px(px(7.0))
                .rounded(px(3.0))
                .text_size(px(11.0))
                .text_color(colors.secondary)
                .flex()
                .items_center()
                .gap(px(5.0))
                // GPUI keeps mouse focus and keyboard focus separate. The
                // former must not leave the black component outline behind;
                // the latter remains discoverable through focus-visible.
                .focus_visible(|trigger| trigger.border_1().border_color(colors.accent))
                .hover(move |trigger| trigger.bg(colors.hover))
                .child(div().flex_none().child(current))
                .child(
                    div()
                        .w(px(16.0))
                        .h(px(28.0))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(colors.secondary)
                        .when(!open, |icon| icon.opacity(0.0))
                        .group_hover("markdown-language-trigger", |icon| icon.opacity(1.0))
                        .child(Icon::new(IconName::ChevronDown).size(px(10.0))),
                )
                .on_click(window.listener_for(&state, CodeLanguage::toggle)),
        );
    if !open {
        return picker;
    }
    picker.child(
        deferred(
            anchored().snap_to_window_with_margin(px(8.0)).child(
                div()
                    .id("markdown-language-popup")
                    .debug_selector(|| "markdown-language-popup".to_owned())
                    .occlude()
                    .w(px(224.0))
                    .max_h(px(280.0))
                    .bg(colors.popup)
                    .text_color(colors.ink)
                    .border_1()
                    .border_color(colors.line)
                    .rounded(px(6.0))
                    .shadow_lg()
                    .p(px(6.0))
                    .child(
                        div().debug_selector(|| "markdown-language-search".to_owned()).child(
                            List::new(&list)
                                    // Small also shrinks the query field. The
                                    // approved prototype gives the search
                                    // field and every menu row independent
                                    // 32px proportions.
                                    .with_size(Size::Medium)
                                    .search_placeholder(search_placeholder)
                                    .scrollbar_visible(false)
                                    .max_h(px(264.0))
                                    .text_color(colors.ink),
                        ),
                    )
                    .on_mouse_down_out(
                        window.listener_for(&state, CodeLanguage::close_from_outside),
                    ),
            ),
        )
        .with_priority(1),
    )
}

/// Construct once per visible root block, not once per frame: extension revisions
/// participate in the Markdown parse cache's identity.
pub(super) fn extensions(
    base: MarkdownExtensions,
    owner: gpui::WeakEntity<TextFileView>,
    block: usize,
) -> MarkdownExtensions {
    base.block_parser(|node, parse| {
        let markdown::mdast::Node::Code(code) = node else { return None };
        // Keep scientific fenced-block ownership with the existing renderer.
        if matches!(code.lang.as_deref(), Some("math" | "latex" | "tex")) {
            return None;
        }
        let position = node.position()?;
        let spec = CodeSpec {
            source: code.value.clone().into(),
            language: code.lang.clone().map(Into::into),
            span: Some((
                position.start.offset + parse.offset(),
                position.end.offset + parse.offset(),
            )),
        };
        Some(
            MarkdownNode::new("pebrel-code-block", spec)
                .text(code.value.clone())
                .markdown(parse.node_source(node).unwrap_or_default().to_owned()),
        )
    })
    .block_renderer("pebrel-code-block", move |node, window, cx| {
        let spec = node.data::<CodeSpec>().expect("registered code block data").clone();
        let hover_group = format!("code-{block}-{}", spec.span.map_or(0, |span| span.0)).into();
        render(owner.clone(), block, spec, hover_group, window, cx)
    })
}

pub(super) fn render(
    owner: gpui::WeakEntity<TextFileView>,
    block: usize,
    code: CodeSpec,
    hover_group: SharedString,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyElement {
    let language = super::super::config::ui_language(cx);
    let markdown = standalone_markdown(&code);
    let plain = SharedString::from(language.text(Message::MarkdownPlainText));
    let current = code
        .language
        .clone()
        .filter(|value| !matches!(value.as_ref(), "text" | "plaintext"))
        .unwrap_or_else(|| plain.clone());
    let current_for_state = current.clone();
    let span = code.span;
    let state = window.use_keyed_state(
        ("code-language-picker", span.map_or(0, |span| span.0)),
        cx,
        |window, cx| {
            let mut choices =
                gpui_component::highlighter::LanguageRegistry::singleton().languages();
            choices.sort();
            choices.insert(0, plain.clone());
            if !choices.contains(&current) {
                choices.push(current.clone());
            }
            let choices = choices.into_iter().map(LanguageOption::new).collect::<Vec<_>>();
            let selected = current_for_state.clone();
            let list = cx.new(|cx| {
                ListState::new(LanguageListDelegate::new(choices, selected.clone()), window, cx)
                    .searchable(true)
            });
            let subscription = cx.subscribe_in(
                &list,
                window,
                move |state: &mut CodeLanguage, _, event, window, cx| match event {
                    ListEvent::Confirm(ix) => {
                        let choice = state
                            .list
                            .read(cx)
                            .delegate()
                            .filtered
                            .get(ix.row)
                            .map(|item| item.name.clone());
                        if let Some(choice) = choice {
                            if let Some((start, end)) = state.span {
                                let choice =
                                    if choice == state.plain { "" } else { choice.as_ref() };
                                let _ = owner.update(cx, |view, cx| {
                                    view.set_preview_language(block, start, end, choice, cx)
                                });
                            }
                        }
                        state.close(window, cx);
                    },
                    ListEvent::Cancel => state.close(window, cx),
                    ListEvent::Select(_) => {},
                },
            );
            let content = cx.new(|cx| TextViewState::markdown(&markdown, cx));
            let copy_feedback = cx.new(|_| CopyFeedback::new());
            let copy_feedback_subscription =
                cx.observe_in(&copy_feedback, window, |_, _, _, cx| cx.notify());
            CodeLanguage {
                list,
                // This handle is explicitly tab-reachable because the
                // trigger tracks it instead of letting the Div create an
                // implicit, non-tab-stop handle.
                focus: cx.focus_handle().tab_stop(true),
                open: false,
                plain,
                selected: current_for_state,
                copy_feedback,
                _copy_feedback_subscription: copy_feedback_subscription,
                span,
                _subscription: subscription,
                content,
                markdown: markdown.clone(),
            }
        },
    );
    // Spans can move after changing a fence; callbacks use the current frame's span.
    state.update(cx, |state, cx| {
        state.span = span;
        if state.selected != current {
            state.selected = current.clone();
            state.list.update(cx, |list, cx| {
                list.delegate_mut().set_selected(current.clone());
                cx.notify();
            });
        }
        if state.markdown != markdown {
            state.content.update(cx, |content, cx| content.set_text(&markdown, cx));
            state.markdown = markdown;
        }
    });
    let content = state.read(cx).content.clone();
    let list = state.read(cx).list.clone();
    let picker_open = state.read(cx).open;
    let picker_focus = state.read(cx).focus.clone();
    let copy_feedback = state.read(cx).copy_feedback.clone();
    let copied = copy_feedback.read(cx).is_copied();
    let source = code.source;
    let colors = code_ui_colors(cx);
    let mut surface = div()
        .id("code-surface")
        .px(px(18.0))
        .py(px(16.0))
        .rounded(px(3.0))
        .bg(colors.code)
        .text_color(colors.ink)
        .text_size(px(13.0))
        .line_height(gpui::relative(1.8))
        .whitespace_nowrap()
        .overflow_x_scroll();
    let style = TextViewStyle {
        code_block: surface.style().clone(),
        highlight_theme: cx.theme().highlight_theme.clone(),
        is_dark: cx.theme().is_dark(),
        ..Default::default()
    };
    v_flex()
        .relative()
        .group(hover_group.clone())
        .w_full()
        .min_w_0()
        .debug_selector(|| "pebrel-code-block".to_owned())
        .child(
            div().w_full().min_w_0().debug_selector(|| "pebrel-code-text".to_owned()).child(
                TextView::new(&content)
                    .w_full()
                    .min_w_0()
                    .max_w_full()
                    .selectable(true)
                    .scrollable(false)
                    .style(style),
            ),
        )
        .child(h_flex().w_full().h(px(28.0)).justify_end().child(render_language_picker(
            state.clone(),
            list,
            current,
            picker_open,
            picker_focus,
            language.text(Message::EditorSearchCodeLanguage),
            window,
            cx,
        )))
        .child(
            div()
                .debug_selector(|| "markdown-copy-code".to_owned())
                .absolute()
                .top(px(8.0))
                .right(px(8.0))
                .invisible()
                .when(copied, |slot| slot.visible())
                .group_hover(hover_group, |slot| slot.visible())
                .child({
                    let feedback = copy_feedback.clone();
                    let button = Button::new("copy-code")
                        .custom(
                            gpui_component::button::ButtonCustomVariant::new(cx)
                                .hover(colors.hover)
                                .active(colors.hover),
                        )
                        .size(px(32.0))
                        .icon(if copied { IconName::Check } else { IconName::Copy })
                        .tooltip(language.text(Message::EditorCopyCode))
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(source.to_string()));
                            feedback.update(cx, |feedback, cx| feedback.mark_copied(cx));
                        });
                    div()
                            // This selector exists only while the checked
                            // state is actually rendered; unlike the outer
                            // invisible hit target it is useful for asserting
                            // feedback expiry in interaction tests.
                            .when(copied, |control| {
                                control.debug_selector(|| "markdown-copy-code-success".to_owned())
                            })
                            .child(button)
                }),
        )
        .into_any_element()
}
