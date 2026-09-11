//! Editable local text files shared by Markdown and code tabs.

mod chrome;
mod code_actions;
#[cfg(all(test, feature = "gpui-test-support"))]
mod code_actions_tests;
mod document;
#[cfg(all(test, feature = "gpui-test-support"))]
mod focus_tests;
mod images;
mod info;
mod outline;
mod outline_view;
mod preview;
mod reader_presentation;
mod source;
#[cfg(all(test, feature = "gpui-test-support"))]
mod tests;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding, ListAlignment,
    ListState, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    PromptLevel, ScrollHandle, SharedString, Subscription, Task, Window, actions, div, px,
};
use gpui_component::text::TextViewState;

use super::prelude::*;
use crate::i18n::Message;
use crate::ssh_sftp::document::{DocumentOperation, RemoteLocation};
use document::SaveError;
use outline::Outline;
use source::{DocumentSource, LoadedDocument};

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
    ReaderFocusChanged { focused: bool },
}

pub struct TextFileView {
    pub path: PathBuf,
    pub title: String,
    input: Entity<InputState>,
    focus: FocusHandle,
    source: DocumentSource,
    document: Option<LoadedDocument>,
    operation: Option<DocumentOperation>,
    dirty: bool,
    loading: bool,
    saving: bool,
    notice: Option<(Message, Option<String>)>,
    markdown: bool,
    preview: bool,
    show_details: bool,
    info: bool,
    outline: Outline,
    blocks: Rc<RefCell<Vec<Option<Entity<TextViewState>>>>>,
    preview_extensions: gpui_component::text::MarkdownExtensions,
    scroll: ListState,
    preview_bounds: Rc<RefCell<Bounds<Pixels>>>,
    preview_selection_scroll_epoch: u64,
    preview_selection_scroll_active: bool,
    outline_scroll: ScrollHandle,
    preview_scrollbar_hovered: bool,
    outline_scrollbar_hovered: bool,
    reader_focus: bool,
    reader_focus_restore: Option<(bool, bool)>,
    details_width: f32,
    details_resize_anchor: Option<(f32, f32)>,
    collapsed_headings: std::collections::HashSet<usize>,
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
        Self::new_with_source(DocumentSource::Local(path), window, cx)
    }

    pub(crate) fn new_remote(
        location: RemoteLocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_source(DocumentSource::Remote(location), window, cx)
    }

    pub(crate) fn source_label(&self) -> String {
        self.source.display()
    }

    pub(crate) fn is_local_path(&self, path: &std::path::Path) -> bool {
        matches!(&self.source, DocumentSource::Local(local) if local == path)
    }

    pub(crate) fn is_remote_location(&self, location: &RemoteLocation) -> bool {
        matches!(&self.source, DocumentSource::Remote(remote) if remote == location)
    }

    fn new_with_source(
        source: DocumentSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let path = source.path().to_owned();
        let language = super::code_tab::language_for_path(&path.to_string_lossy());
        // Remote Markdown is edited as source. It must not resolve remote image
        // paths through the local document preview's filesystem loader.
        let markdown = matches!(language, "markdown") && !source.is_remote();
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
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let title = match &source {
            DocumentSource::Local(_) => name,
            DocumentSource::Remote(location) => format!("{name} · {}", location.destination),
        };
        let preview_extensions = preview::extensions(path.parent().map(PathBuf::from));
        let mut this = Self {
            preview_extensions,
            path,
            title,
            input,
            focus: cx.focus_handle(),
            source,
            document: None,
            operation: None,
            dirty: false,
            loading: false,
            saving: false,
            notice: None,
            markdown,
            preview: markdown,
            show_details: markdown,
            info: false,
            outline: Outline::default(),
            blocks: Rc::default(),
            scroll: ListState::new(0, ListAlignment::Top, px(500.0)),
            preview_bounds: Rc::default(),
            preview_selection_scroll_epoch: 0,
            preview_selection_scroll_active: false,
            outline_scroll: ScrollHandle::new(),
            preview_scrollbar_hovered: false,
            outline_scrollbar_hovered: false,
            reader_focus: false,
            reader_focus_restore: None,
            details_width: reader_presentation::OUTLINE_WIDTH,
            details_resize_anchor: None,
            collapsed_headings: Default::default(),
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

    pub(super) fn reader_focus(&self) -> bool {
        self.reader_focus
    }

    /// Clear reader focus while switching tabs or closing a document. The
    /// workspace owns the outer sidebar snapshot, so this method only restores
    /// this document's own details panel and intentionally emits no event.
    pub(super) fn clear_reader_focus(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.reader_focus {
            return false;
        }
        self.reader_focus = false;
        if let Some((show_details, info)) = self.reader_focus_restore.take() {
            self.show_details = show_details;
            self.info = info;
        }
        self.details_resize_anchor = None;
        cx.notify();
        true
    }

    fn toggle_reader_focus(&mut self, cx: &mut Context<Self>) {
        if !self.markdown {
            return;
        }
        if self.reader_focus {
            self.clear_reader_focus(cx);
        } else {
            self.reader_focus = true;
            self.reader_focus_restore = Some((self.show_details, self.info));
            self.show_details = false;
            self.details_resize_anchor = None;
        }
        cx.emit(TextFileEvent::ReaderFocusChanged { focused: self.reader_focus });
        cx.notify();
    }

    fn begin_details_resize(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        if !self.show_details || !self.markdown {
            return;
        }
        self.details_resize_anchor = Some((f32::from(event.position.x), self.details_width));
        cx.notify();
    }

    fn update_details_resize(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some((start_x, start_width)) = self.details_resize_anchor else { return };
        if event.pressed_button != Some(MouseButton::Left) {
            self.details_resize_anchor = None;
            cx.notify();
            return;
        }
        // The divider is on the panel's left edge: moving left grows the panel.
        let width = reader_presentation::clamp_details_width(
            start_width + (start_x - f32::from(event.position.x)),
        );
        if (width - self.details_width).abs() >= 0.5 {
            self.details_width = width;
            cx.notify();
        }
    }

    fn finish_details_resize(&mut self, _event: &MouseUpEvent, cx: &mut Context<Self>) {
        if self.details_resize_anchor.take().is_some() {
            cx.notify();
        }
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
        let source = self.source.clone();
        let operation = DocumentOperation::default();
        if let Some(previous) = self.operation.replace(operation.clone()) {
            previous.cancel();
        }
        let markdown = self.markdown;
        let resources = super::scientific_render::assets(cx);
        let task = cx.background_executor().spawn(async move {
            let _permit = resources.document_permit().await;
            source.load(operation).await.map(|doc| {
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
                    view.operation = None;
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
                            view.notice = Some(if error.kind() == std::io::ErrorKind::Interrupted {
                                (Message::TransferCancelled, None)
                            } else {
                                (Message::EditorReadFailed, Some(error.to_string()))
                            })
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
            Some(&self.source.display()),
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
        let source = self.source.clone();
        let operation = DocumentOperation::default();
        self.operation = Some(operation.clone());
        self.saving = true;
        self.notice = None;
        let task = cx
            .background_executor()
            .spawn(async move { document.save(source, text, operation).await });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |view, cx| {
                view.saving = false;
                view.operation = None;
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
                            SaveError::Io(error)
                                if error.kind() == std::io::ErrorKind::Interrupted =>
                            {
                                (Message::TransferCancelled, None)
                            },
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
        let resources = super::scientific_render::assets(cx);
        self.preview_task = Some(cx.spawn(async move |this, cx| {
            executor.timer(Duration::from_millis(250)).await;
            let Ok((source, path)) =
                this.update(cx, |view, cx| (view.input.read(cx).value(), view.path.clone()))
            else {
                return;
            };
            let outline = executor
                .spawn(async move {
                    let _permit = resources.document_permit().await;
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
        self.blocks = Rc::new(RefCell::new(vec![None; outline.blocks.len()]));
        self.scroll.reset(outline.blocks.len());
        self.outline = outline;
        self.collapsed_headings.clear();
        self.selected_heading = None;
        self.all_selected = false;
        cx.notify();
    }

    fn toggle_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.preview = !self.preview;
        if !self.preview {
            self.stop_preview_selection_scroll();
            for block in self.blocks.borrow_mut().iter_mut() {
                *block = None;
            }
            self.all_selected = false;
            self.input.update(cx, |input, cx| input.focus(window, cx));
        }
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
}

impl Render for TextFileView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let language = super::config::ui_language(cx);
        let muted = cx.theme().muted_foreground;
        let editable = !self.loading && self.document.as_ref().is_some_and(|doc| !doc.read_only);
        let mut notice = if self.loading {
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
        if self.preview && self.outline.limited {
            let detail = language.text(Message::EditorPreviewLimited);
            notice = Some(
                notice.map_or_else(|| detail.to_owned(), |notice| format!("{notice} {detail}")),
            );
        }
        let content = if self.preview {
            self.render_markdown_preview(cx)
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
            .relative()
            .overflow_hidden()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                // GPUI can deliver a move after the pointer leaves the
                // divider with no pressed button. Clear only an active resize;
                // normal reader selection remains untouched.
                if this.details_resize_anchor.is_some()
                    && event.pressed_button != Some(MouseButton::Left)
                {
                    this.details_resize_anchor = None;
                    cx.notify();
                }
            }))
            .capture_any_mouse_up(cx.listener(|this, event: &MouseUpEvent, _, cx| {
                if this.details_resize_anchor.is_some() && event.button == MouseButton::Left {
                    this.finish_details_resize(event, cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &SaveFile, _, cx| {
                this.save(cx).detach();
            }))
            .when(self.preview, |root| {
                root.capture_action(cx.listener(
                    |this, _: &gpui_component::input::SelectAll, _, cx| {
                        for block in this.blocks.borrow().iter().flatten() {
                            block.update(cx, |state, cx| state.select_all(cx));
                        }
                        this.all_selected = true;
                        cx.stop_propagation();
                        cx.notify();
                    },
                ))
                .capture_action(cx.listener(|this, _: &gpui_component::input::Copy, _, cx| {
                    if this.all_selected {
                        // Do not materialize off-screen preview blocks for copy.
                        let text = this.input.read(cx).value().to_string();
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
            .child(self.render_toolbar(editable, window, cx))
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
            .when(self.details_resize_anchor.is_some(), |root| {
                root.child(
                    div()
                        .id("file-details-resize-overlay")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .cursor_col_resize()
                        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                            this.update_details_resize(event, cx);
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseUpEvent, _, cx| {
                                this.finish_details_resize(event, cx);
                            }),
                        ),
                )
            })
    }
}

impl Drop for TextFileView {
    fn drop(&mut self) {
        if let Some(operation) = &self.operation {
            operation.cancel();
        }
    }
}
