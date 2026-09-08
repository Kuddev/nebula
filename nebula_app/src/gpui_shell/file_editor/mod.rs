//! Editable local text files shared by Markdown and code tabs.

mod document;
mod images;
mod info;
mod outline;
#[cfg(all(test, feature = "gpui-test-support"))]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding, ListAlignment,
    ListState, MouseButton, Pixels, Point, PromptLevel, SharedString, Subscription, Task, Window,
    actions, div, px,
};
use gpui_component::WindowExt as _;
use gpui_component::text::{TextView, TextViewState, TextViewStyle};

use super::prelude::*;
use crate::i18n::Message;
use document::{Document, SaveError};
use outline::Outline;

actions!(file_editor, [SaveFile]);

pub(super) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-s", SaveFile, Some("FileEditor")),
        KeyBinding::new("cmd-s", SaveFile, Some("FileEditor")),
        KeyBinding::new("ctrl-a", gpui_component::input::SelectAll, Some("FileEditor")),
        KeyBinding::new("cmd-a", gpui_component::input::SelectAll, Some("FileEditor")),
        KeyBinding::new("ctrl-c", gpui_component::input::Copy, Some("FileEditor")),
        KeyBinding::new("cmd-c", gpui_component::input::Copy, Some("FileEditor")),
    ]);
}

pub enum TextFileEvent {
    Changed,
    SelectionContextMenuRequested { position: Point<Pixels>, text: String },
}

pub struct TextFileView {
    pub path: PathBuf,
    pub title: String,
    input: Entity<InputState>,
    focus: FocusHandle,
    document: Option<Document>,
    dirty: bool,
    loading: bool,
    saving: bool,
    notice: Option<(Message, Option<String>)>,
    markdown: bool,
    preview: bool,
    show_details: bool,
    info: bool,
    outline: Outline,
    blocks: Vec<Entity<TextViewState>>,
    scroll: ListState,
    selected_heading: Option<usize>,
    all_selected: bool,
    revision: u64,
    preview_task: Option<Task<()>>,
    _input_subscription: Subscription,
}

impl EventEmitter<TextFileEvent> for TextFileView {}

impl Focusable for TextFileView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.preview { self.focus.clone() } else { self.input.read(cx).focus_handle(cx) }
    }
}

impl TextFileView {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let language = super::code_tab::language_for_path(&path.to_string_lossy());
        let markdown = matches!(language, "markdown");
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(language)
                .line_number(true)
                .indent_guides(true)
                .soft_wrap(false)
        });
        let subscription = cx.subscribe_in(&input, window, |this, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.dirty = this
                    .document
                    .as_ref()
                    .is_some_and(|doc| this.input.read(cx).value().as_ref() != doc.text);
                if this.markdown {
                    this.schedule_preview(cx);
                }
                cx.emit(TextFileEvent::Changed);
                cx.notify();
            }
        });
        let title = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let mut this = Self {
            path,
            title,
            input,
            focus: cx.focus_handle(),
            document: None,
            dirty: false,
            loading: false,
            saving: false,
            notice: None,
            markdown,
            preview: markdown,
            show_details: markdown,
            info: false,
            outline: Outline::default(),
            blocks: vec![],
            scroll: ListState::new(0, ListAlignment::Top, px(500.0)),
            selected_heading: None,
            all_selected: false,
            revision: 0,
            preview_task: None,
            _input_subscription: subscription,
        };
        this.reload(window, cx);
        this
    }

    pub(super) fn draft(&self, cx: &App) -> SharedString {
        self.input.read(cx).value()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
    pub fn is_saving(&self) -> bool {
        self.saving
    }

    pub fn tab_title(&self) -> String {
        if self.dirty { format!("{} •", self.title) } else { self.title.clone() }
    }

    /// Reopening the same path must preserve a draft and its undo history.
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dirty || self.saving || self.loading {
            return;
        }
        self.load(window, cx);
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = true;
        self.revision += 1;
        self.preview_task = None;
        let path = self.path.clone();
        let markdown = self.markdown;
        let task = cx.background_executor().spawn(async move {
            Document::load(&path).map(|doc| {
                let outline = markdown
                    .then(|| Outline::parse(&images::rewrite_doc_images(&doc.text, path.parent())));
                (doc, outline)
            })
        });
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let loaded = task.await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |view, cx| {
                    view.loading = false;
                    match loaded {
                        Ok((document, outline)) => {
                            view.input.update(cx, |input, cx| {
                                input.set_value(document.text.clone(), window, cx)
                            });
                            view.document = Some(document);
                            view.dirty = false;
                            view.notice = None;
                            if let Some(outline) = outline {
                                view.apply_outline(outline, cx);
                            }
                        },
                        Err(error) => {
                            view.notice = Some((Message::EditorReadFailed, Some(error.to_string())))
                        },
                    }
                    cx.emit(TextFileEvent::Changed);
                    cx.notify();
                });
            });
        })
        .detach();
        cx.notify();
    }

    fn request_reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || self.loading {
            return;
        }
        if !self.dirty {
            self.load(window, cx);
            return;
        }
        let language = super::config::ui_language(cx);
        let answer = window.prompt(
            PromptLevel::Warning,
            language.text(Message::EditorDiscardTitle),
            Some(&self.path.display().to_string()),
            &[language.text(Message::EditorCancel), language.text(Message::EditorDiscard)],
            cx,
        );
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(1) {
                return;
            }
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |view, cx| view.load(window, cx));
            });
        })
        .detach();
    }

    /// A successful save only cleans the exact snapshot written. Edits made while
    /// I/O is running remain dirty; a second write cannot race the first one.
    pub fn save(&mut self, cx: &mut Context<Self>) -> Task<bool> {
        if self.saving || self.loading {
            return Task::ready(false);
        }
        if !self.dirty {
            return Task::ready(true);
        }
        let Some(document) = self.document.clone() else { return Task::ready(false) };
        let text = self.input.read(cx).value().to_string();
        let path = self.path.clone();
        self.saving = true;
        self.notice = None;
        let task = cx.background_executor().spawn(async move { document.save(&path, text) });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |view, cx| {
                view.saving = false;
                let success = match result {
                    Ok(document) => {
                        view.dirty = view.input.read(cx).value().as_ref() != document.text;
                        view.document = Some(document);
                        view.notice = Some((Message::EditorSaved, None));
                        !view.dirty
                    },
                    Err(error) => {
                        view.notice = Some(match error {
                            SaveError::Changed => (Message::EditorConflict, None),
                            SaveError::ReadOnly => (Message::EditorReadOnly, None),
                            SaveError::Io(error) => {
                                (Message::EditorSaveFailed, Some(error.to_string()))
                            },
                        });
                        false
                    },
                };
                cx.emit(TextFileEvent::Changed);
                cx.notify();
                success
            })
            .unwrap_or(false)
        })
    }

    fn schedule_preview(&mut self, cx: &mut Context<Self>) {
        self.revision += 1;
        let revision = self.revision;
        let executor = cx.background_executor().clone();
        self.preview_task = Some(cx.spawn(async move |this, cx| {
            executor.timer(Duration::from_millis(250)).await;
            let Ok((source, path)) =
                this.update(cx, |view, cx| (view.input.read(cx).value(), view.path.clone()))
            else {
                return;
            };
            let outline = executor
                .spawn(async move {
                    Outline::parse(&images::rewrite_doc_images(&source, path.parent()))
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.revision == revision {
                    view.apply_outline(outline, cx);
                }
            });
        }));
    }

    fn apply_outline(&mut self, outline: Outline, cx: &mut Context<Self>) {
        self.blocks = outline
            .blocks
            .iter()
            .map(|text| cx.new(|cx| TextViewState::markdown(text, cx)))
            .collect();
        self.scroll.reset(self.blocks.len());
        self.outline = outline;
        self.selected_heading = None;
        self.all_selected = false;
        cx.notify();
    }

    fn jump_to_heading(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(heading) = self.outline.headings.get(index) else { return };
        self.selected_heading = Some(index);
        if self.preview {
            self.scroll
                .scroll_to(gpui::ListOffset { item_ix: heading.block, offset_in_item: px(0.0) });
        } else {
            let row = heading.row;
            self.input.update(cx, |input, cx| {
                input.set_cursor_position(
                    gpui_component::input::Position { line: row, character: 0 },
                    window,
                    cx,
                )
            });
        }
        cx.notify();
    }

    fn render_outline(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let language = super::config::ui_language(cx);
        let muted = cx.theme().muted_foreground;
        v_flex()
            .id("markdown-outline")
            .w(px(210.0))
            .min_w(px(110.0))
            .max_w(gpui::relative(0.32))
            .h_full()
            .flex_shrink_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .child(
                div().p_3().text_xs().font_semibold().child(language.text(Message::EditorOutline)),
            )
            .child(
                v_flex()
                    .id("markdown-headings")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_2()
                    .when(self.outline.headings.is_empty(), |list| {
                        list.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(language.text(Message::EditorNoHeadings)),
                        )
                    })
                    .children(self.outline.headings.iter().enumerate().map(|(index, heading)| {
                        let copied = heading.label.clone();
                        div()
                            .id(("markdown-heading", index))
                            .min_h(px(28.0))
                            .py_1()
                            .pr_2()
                            .pl(px(8.0 + heading.depth.saturating_sub(1) as f32 * 12.0))
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .text_color(if self.selected_heading == Some(index) {
                                cx.theme().link
                            } else {
                                muted
                            })
                            .hover(|style| style.bg(cx.theme().list_hover))
                            .child(heading.label.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.jump_to_heading(index, window, cx)
                            }))
                            .context_menu(move |menu, _, _| {
                                let copied = copied.clone();
                                menu.item(
                                    gpui_component::menu::PopupMenuItem::new(
                                        language.text(Message::EditorCopyHeading),
                                    )
                                    .icon(IconName::Copy)
                                    .on_click(
                                        move |_, _, cx| {
                                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                                copied.clone(),
                                            ));
                                        },
                                    ),
                                )
                            })
                    })),
            )
            .into_any_element()
    }
}

impl Render for TextFileView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let language = super::config::ui_language(cx);
        let muted = cx.theme().muted_foreground;
        let editable = !self.loading && self.document.as_ref().is_some_and(|doc| !doc.read_only);
        let path = self.path.display().to_string();
        let notice = if self.loading {
            Some(language.text(Message::EditorLoading).to_owned())
        } else if let Some((message, error)) = &self.notice {
            Some(language.format(*message, &[("error", error.as_deref().unwrap_or_default())]))
        } else {
            self.document.as_ref().and_then(|doc| {
                let message = if doc.truncated {
                    Message::EditorTruncated
                } else if doc.invalid_encoding {
                    Message::EditorEncoding
                } else if doc.read_only {
                    Message::EditorReadOnly
                } else {
                    return None;
                };
                Some(language.text(message).to_owned())
            })
        };
        let content = if self.preview {
            let blocks = self.blocks.clone();
            let style = TextViewStyle {
                image_base: self.path.parent().map(Arc::from),
                highlight_theme: cx.theme().highlight_theme.clone(),
                is_dark: cx.theme().is_dark(),
                ..Default::default()
            };
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .px_4()
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
                    gpui::list(self.scroll.clone(), move |index, _, _| {
                        div()
                            .w_full()
                            .py_1()
                            .child(
                                TextView::new(&blocks[index])
                                    .selectable(true)
                                    .scrollable(false)
                                    .style(style.clone()),
                            )
                            .into_any_element()
                    })
                    .size_full(),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(
                    Input::new(&self.input)
                        .h_full()
                        .disabled(!editable)
                        .bordered(false)
                        .focus_bordered(false)
                        .rounded(px(0.0))
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_size(cx.theme().mono_font_size)
                        .line_height(gpui::relative(1.55))
                        .px_3()
                        .py_2(),
                )
                .into_any_element()
        };
        v_flex()
            .id("text-file-editor")
            .key_context("FileEditor")
            .track_focus(&self.focus)
            .size_full()
            .overflow_hidden()
            .on_action(cx.listener(|this, _: &SaveFile, _, cx| {
                this.save(cx).detach();
            }))
            .when(self.preview, |root| {
                root.capture_action(cx.listener(
                    |this, _: &gpui_component::input::SelectAll, _, cx| {
                        for block in &this.blocks {
                            block.update(cx, |state, cx| state.select_all(cx));
                        }
                        this.all_selected = true;
                        cx.stop_propagation();
                        cx.notify();
                    },
                ))
                .capture_action(cx.listener(|this, _: &gpui_component::input::Copy, _, cx| {
                    if this.all_selected {
                        let text = this
                            .blocks
                            .iter()
                            .map(|block| block.read(cx).selected_text())
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                        cx.stop_propagation();
                    } else {
                        cx.propagate();
                    }
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.all_selected = false;
                        cx.notify();
                    }),
                )
            })
            .child(
                h_flex()
                    .h(px(36.0))
                    .flex_shrink_0()
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div().flex_1().min_w_0().truncate().text_xs().text_color(muted).child(path),
                    )
                    .when(self.dirty, |bar| {
                        bar.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().warning)
                                .child(language.text(Message::EditorUnsaved)),
                        )
                    })
                    .when(self.markdown, |bar| {
                        bar.child(
                            Button::new("file-toggle-preview")
                                .ghost()
                                .xsmall()
                                .label(language.text(if self.preview {
                                    Message::EditorEdit
                                } else {
                                    Message::EditorPreview
                                }))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.preview = !this.preview;
                                    if !this.preview {
                                        this.input.update(cx, |input, cx| input.focus(window, cx));
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("file-toggle-outline")
                                .ghost()
                                .xsmall()
                                .selected(self.show_details)
                                .label(language.text(Message::EditorOutline))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_details = !this.show_details || this.info;
                                    this.info = false;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        Button::new("file-info")
                            .ghost()
                            .xsmall()
                            .label(language.text(Message::EditorInfo))
                            .selected(self.show_details && self.info)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_details = !this.show_details || !this.info;
                                this.info = true;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("file-reload")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Redo2)
                            .tooltip(language.text(Message::EditorReload))
                            .disabled(self.saving || self.loading)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.request_reload(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("file-save")
                            .small()
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
                    ),
            )
            .when_some(notice, |root, notice| {
                root.child(div().px_3().py_1().text_xs().text_color(muted).child(notice))
            })
            .child(h_flex().flex_1().min_h_0().w_full().items_start().child(content).when(
                self.show_details,
                |row| {
                    row.child(if self.info {
                        self.render_info(cx)
                    } else {
                        self.render_outline(cx)
                    })
                },
            ))
    }
}
