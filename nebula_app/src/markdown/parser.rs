//! Markdown text → [`FormattedText`]: folds the pulldown-cmark event stream
//! (CommonMark + GFM tables / task lists / strikethrough) into the flat
//! block list defined in `super`. Parsing never fails — any input produces a
//! best-effort document — so the viewer has no error path to render.
//!
//! This file owns ONLY the event folding. The AST lives in `super`, the
//! on-screen layout (wrapping, virtual scrolling) in `display::markdown_view`.

use pulldown_cmark::{
    Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd,
};

use super::{
    CodeBlockText, CustomWeight, FormattedImage, FormattedIndentTextInline, FormattedTable,
    FormattedTaskList, FormattedText, FormattedTextFragment, FormattedTextHeader,
    FormattedTextInline, FormattedTextLine, FormattedTextStyles, Hyperlink,
    OrderedFormattedIndentTextInline, TableAlignment,
};

/// Parse a Markdown document. GFM tables, task lists and strikethrough are
/// always on; a YAML front-matter block is recognized and skipped.
pub fn parse_markdown(text: &str) -> FormattedText {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);

    let mut fold = Fold::default();
    for event in Parser::new_ext(text, options) {
        fold.event(event);
    }
    fold.finish()
}

/// A list item whose first-line text is still being collected. Flushed into
/// an Ordered/Unordered/Task list block by the item's end — or earlier, when
/// a nested block (sub-list, code block) interrupts the item.
struct PendingItem {
    /// `Some(n)` when the enclosing list is ordered: this item's number.
    number: Option<u64>,
    /// Set by a GFM `TaskListMarker` right after the item opens.
    task: Option<bool>,
    /// Nesting depth of the enclosing list (0 = top level).
    depth: usize,
}

/// In-flight GFM table state between `Start(Table)` and `End(Table)`.
struct TableFold {
    alignments: Vec<TableAlignment>,
    headers: Vec<FormattedTextInline>,
    rows: Vec<Vec<FormattedTextInline>>,
    current_row: Vec<FormattedTextInline>,
    in_head: bool,
}

/// Event-stream folding state. One instance per document.
#[derive(Default)]
struct Fold {
    out: Vec<FormattedTextLine>,
    /// Inline run being collected for the current paragraph / heading / list
    /// item / table cell.
    inline: Vec<FormattedTextFragment>,
    // Inline style state: emphasis nests, so these are depths, not flags.
    strong: usize,
    emphasis: usize,
    strike: usize,
    /// Stack of open link destinations; the innermost wins.
    links: Vec<String>,
    /// Stack of open lists: `Some(next_number)` for ordered, `None` for
    /// bullets. Depth = index in this stack.
    list_stack: Vec<Option<u64>>,
    pending_item: Option<PendingItem>,
    /// `>` quote nesting around the block currently being produced.
    quote_depth: usize,
    heading: Option<usize>,
    code: Option<CodeBlockText>,
    table: Option<TableFold>,
    /// Collecting an image's alt text between `Start(Image)` and its end.
    image: Option<FormattedImage>,
    /// Inside the YAML front-matter block: all text is dropped.
    in_metadata: bool,
}

impl Fold {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if self.in_metadata {
                    return;
                }
                if let Some(code) = &mut self.code {
                    code.code.push_str(&text);
                } else if let Some(image) = &mut self.image {
                    image.alt_text.push_str(&text);
                } else {
                    self.push_text(&text, false);
                }
            },
            Event::Code(text) => {
                if let Some(image) = &mut self.image {
                    image.alt_text.push_str(&text);
                } else {
                    self.push_text(&text, true);
                }
            },
            // The viewer renders HTML as the literal text the author wrote.
            Event::Html(html) | Event::InlineHtml(html) => self.push_text(&html, false),
            Event::SoftBreak => self.push_text(" ", false),
            // A hard break stays inside the paragraph block: the layout pass
            // turns embedded '\n' into a forced visual line break.
            Event::HardBreak => self.push_text("\n", false),
            Event::Rule => {
                self.flush_inline_as_paragraph();
                self.push_block(FormattedTextLine::HorizontalRule);
            },
            Event::TaskListMarker(complete) => {
                if let Some(item) = &mut self.pending_item {
                    item.task = Some(complete);
                }
            },
            Event::FootnoteReference(name) => {
                self.push_text(&format!("[^{name}]"), false);
            },
            Event::InlineMath(math) | Event::DisplayMath(math) => {
                self.push_text(&math, true);
            },
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph | Tag::HtmlBlock => {},
            Tag::Heading { level, .. } => {
                self.flush_inline_as_paragraph();
                self.heading = Some(level as usize);
            },
            Tag::BlockQuote(_) => {
                self.flush_inline_as_paragraph();
                self.quote_depth += 1;
            },
            Tag::CodeBlock(kind) => {
                // A code block opening inside a list item ends the item's
                // first-line text.
                self.flush_pending_item();
                self.flush_inline_as_paragraph();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_owned(),
                    CodeBlockKind::Indented => String::new(),
                };
                let lang = if lang.is_empty() { "text".to_owned() } else { lang };
                self.code = Some(CodeBlockText { lang, code: String::new() });
            },
            Tag::List(start) => {
                // A sub-list opening inside an item ends that item's text.
                self.flush_pending_item();
                self.flush_inline_as_paragraph();
                self.list_stack.push(start);
            },
            Tag::Item => {
                let depth = self.list_stack.len().saturating_sub(1);
                let number = self.list_stack.last_mut().and_then(|slot| {
                    let number = *slot;
                    if let Some(n) = slot {
                        *n += 1;
                    }
                    number
                });
                self.pending_item = Some(PendingItem { number, task: None, depth });
            },
            Tag::Table(alignments) => {
                self.flush_inline_as_paragraph();
                self.table = Some(TableFold {
                    alignments: alignments
                        .into_iter()
                        .map(|alignment| match alignment {
                            Alignment::Center => TableAlignment::Center,
                            Alignment::Right => TableAlignment::Right,
                            Alignment::None | Alignment::Left => TableAlignment::Left,
                        })
                        .collect(),
                    headers: Vec::new(),
                    rows: Vec::new(),
                    current_row: Vec::new(),
                    in_head: false,
                });
            },
            Tag::TableHead => {
                if let Some(table) = &mut self.table {
                    table.in_head = true;
                }
            },
            Tag::TableRow | Tag::TableCell => {},
            Tag::Emphasis => self.emphasis += 1,
            Tag::Strong => self.strong += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { dest_url, .. } => self.links.push(dest_url.into_string()),
            Tag::Image { dest_url, title, .. } => {
                self.image = Some(FormattedImage {
                    alt_text: String::new(),
                    source: dest_url.into_string(),
                    title: (!title.is_empty()).then(|| title.into_string()),
                });
            },
            Tag::MetadataBlock(_) => self.in_metadata = true,
            // Footnote bodies render inline where they appear (rare in the
            // files this viewer targets); no dedicated block.
            Tag::FootnoteDefinition(_) => {},
            _ => {},
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                // A loose list item's first paragraph IS the item text.
                if self.pending_item.is_some() {
                    self.flush_pending_item();
                } else {
                    self.flush_inline_as_paragraph();
                }
            },
            TagEnd::Heading(_) => {
                let heading_size = self.heading.take().unwrap_or(1);
                let text = std::mem::take(&mut self.inline);
                self.push_block(FormattedTextLine::Heading(FormattedTextHeader {
                    heading_size,
                    text,
                }));
            },
            TagEnd::BlockQuote(_) => {
                self.flush_inline_as_paragraph();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            },
            TagEnd::CodeBlock => {
                if let Some(mut code) = self.code.take() {
                    // The final newline is fence syntax, not content.
                    if code.code.ends_with('\n') {
                        code.code.pop();
                    }
                    self.push_block(FormattedTextLine::CodeBlock(code));
                }
            },
            TagEnd::List(_) => {
                self.list_stack.pop();
            },
            TagEnd::Item => self.flush_pending_item(),
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    let mut alignments = table.alignments;
                    alignments.resize(table.headers.len().max(1), TableAlignment::Left);
                    self.push_block(FormattedTextLine::Table(FormattedTable {
                        headers: table.headers,
                        alignments,
                        rows: table.rows,
                    }));
                }
            },
            TagEnd::TableHead => {
                if let Some(table) = &mut self.table {
                    table.in_head = false;
                }
            },
            TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.current_row);
                    table.rows.push(row);
                }
            },
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.inline);
                if let Some(table) = &mut self.table {
                    if table.in_head {
                        table.headers.push(cell);
                    } else {
                        table.current_row.push(cell);
                    }
                }
            },
            TagEnd::Emphasis => self.emphasis = self.emphasis.saturating_sub(1),
            TagEnd::Strong => self.strong = self.strong.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => {
                self.links.pop();
            },
            TagEnd::Image => {
                if let Some(image) = self.image.take() {
                    // Images are block-level in this viewer. One mid-paragraph
                    // splits the paragraph — acceptable for the target files.
                    self.flush_inline_as_paragraph();
                    self.push_block(FormattedTextLine::Image(image));
                }
            },
            TagEnd::MetadataBlock(_) => self.in_metadata = false,
            TagEnd::HtmlBlock => self.flush_inline_as_paragraph(),
            _ => {},
        }
    }

    /// Append text to the current inline run under the active styles,
    /// merging into the previous fragment when the styles are identical.
    fn push_text(&mut self, text: &str, inline_code: bool) {
        if text.is_empty() || self.in_metadata {
            return;
        }
        let styles = FormattedTextStyles {
            weight: (self.strong > 0).then_some(CustomWeight::Bold),
            italic: self.emphasis > 0,
            underline: false,
            strikethrough: self.strike > 0,
            inline_code,
            hyperlink: self.links.last().map(|url| Hyperlink::Url(url.clone())),
        };
        if let Some(last) = self.inline.last_mut() {
            if last.styles == styles {
                last.text.push_str(text);
                return;
            }
        }
        self.inline.push(FormattedTextFragment { text: text.to_owned(), styles });
    }

    /// Emit the pending inline run as a plain paragraph block, if any.
    fn flush_inline_as_paragraph(&mut self) {
        if self.inline.is_empty() {
            return;
        }
        let inline = std::mem::take(&mut self.inline);
        self.push_block(FormattedTextLine::Line(inline));
    }

    /// Emit the open list item (if one is collecting) as its list block.
    fn flush_pending_item(&mut self) {
        let Some(item) = self.pending_item.take() else { return };
        let text = std::mem::take(&mut self.inline);
        let line = match item.task {
            Some(complete) => FormattedTextLine::TaskList(FormattedTaskList {
                complete,
                indent_level: item.depth,
                text,
            }),
            None => {
                let indented_text =
                    FormattedIndentTextInline { indent_level: item.depth, text };
                match item.number {
                    Some(number) => {
                        FormattedTextLine::OrderedList(OrderedFormattedIndentTextInline {
                            number: Some(number as usize),
                            indented_text,
                        })
                    },
                    None => FormattedTextLine::UnorderedList(indented_text),
                }
            },
        };
        self.push_block(line);
    }

    /// Push a completed block, wrapping it in the active quote depth.
    fn push_block(&mut self, line: FormattedTextLine) {
        let line = if self.quote_depth > 0 {
            FormattedTextLine::Quote { depth: self.quote_depth, line: Box::new(line) }
        } else {
            line
        };
        self.out.push(line);
    }

    fn finish(mut self) -> FormattedText {
        self.flush_inline_as_paragraph();
        FormattedText { lines: self.out.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Vec<FormattedTextLine> {
        parse_markdown(text).lines.into_iter().collect()
    }

    fn plain(text: &str) -> FormattedTextFragment {
        FormattedTextFragment::plain_text(text)
    }

    #[test]
    fn heading_paragraph_and_inline_styles() {
        let lines = parse("# Title\n\nplain **bold** *it* `code` ~~gone~~");
        assert_eq!(
            lines[0],
            FormattedTextLine::Heading(FormattedTextHeader {
                heading_size: 1,
                text: vec![plain("Title")],
            })
        );
        let FormattedTextLine::Line(inline) = &lines[1] else {
            panic!("expected paragraph, got {:?}", lines[1]);
        };
        assert_eq!(inline[0], plain("plain "));
        assert_eq!(inline[1], FormattedTextFragment::bold("bold"));
        assert_eq!(inline[2], plain(" "));
        assert_eq!(inline[3], FormattedTextFragment::italic("it"));
        assert_eq!(inline[4], plain(" "));
        assert_eq!(inline[5], FormattedTextFragment::inline_code("code"));
        assert!(inline[7].styles.strikethrough);
        assert_eq!(inline[7].text, "gone");
    }

    #[test]
    fn links_carry_their_target() {
        let lines = parse("see [docs](https://example.com) now");
        let FormattedTextLine::Line(inline) = &lines[0] else { panic!() };
        assert_eq!(inline[1], FormattedTextFragment::hyperlink("docs", "https://example.com"));
        assert_eq!(lines[0].hyperlinks(), vec![(4..8, "https://example.com".to_owned())]);
    }

    #[test]
    fn fenced_code_block_keeps_lang_and_content() {
        let lines = parse("```rust\nfn main() {}\nlet x = 1;\n```\n");
        assert_eq!(
            lines[0],
            FormattedTextLine::CodeBlock(CodeBlockText {
                lang: "rust".into(),
                code: "fn main() {}\nlet x = 1;".into(),
            })
        );
    }

    #[test]
    fn unlabelled_code_block_defaults_to_text() {
        let lines = parse("```\nhi\n```\n");
        let FormattedTextLine::CodeBlock(block) = &lines[0] else { panic!() };
        assert_eq!(block.lang, "text");
    }

    #[test]
    fn nested_lists_carry_indent_levels() {
        let lines = parse("- a\n  - b\n- c\n");
        let levels: Vec<(usize, String)> = lines
            .iter()
            .filter_map(|line| match line {
                FormattedTextLine::UnorderedList(list) => {
                    Some((list.indent_level, line.raw_text().trim().to_owned()))
                },
                _ => None,
            })
            .collect();
        assert_eq!(levels, vec![(0, "a".into()), (1, "b".into()), (0, "c".into())]);
    }

    #[test]
    fn ordered_lists_number_their_items() {
        let lines = parse("3. three\n4. four\n");
        let numbers: Vec<Option<usize>> = lines
            .iter()
            .filter_map(|line| match line {
                FormattedTextLine::OrderedList(item) => Some(item.number),
                _ => None,
            })
            .collect();
        assert_eq!(numbers, vec![Some(3), Some(4)]);
    }

    #[test]
    fn task_lists_keep_completion_state() {
        let lines = parse("- [x] done\n- [ ] todo\n");
        let states: Vec<bool> = lines
            .iter()
            .filter_map(|line| match line {
                FormattedTextLine::TaskList(task) => Some(task.complete),
                _ => None,
            })
            .collect();
        assert_eq!(states, vec![true, false]);
    }

    #[test]
    fn block_quotes_wrap_inner_blocks_with_depth() {
        let lines = parse("> quoted\n>> deeper\n");
        let FormattedTextLine::Quote { depth: 1, line } = &lines[0] else {
            panic!("expected depth-1 quote, got {:?}", lines[0]);
        };
        assert!(matches!(line.as_ref(), FormattedTextLine::Line(_)));
        assert!(matches!(&lines[1], FormattedTextLine::Quote { depth: 2, .. }));
    }

    #[test]
    fn tables_capture_alignment_and_cells() {
        let lines = parse("| a | b |\n|:-:|--:|\n| 1 | 2 |\n");
        let FormattedTextLine::Table(table) = &lines[0] else { panic!() };
        assert_eq!(table.alignments, vec![TableAlignment::Center, TableAlignment::Right]);
        assert_eq!(table.headers.len(), 2);
        assert_eq!(table.rows, vec![vec![vec![plain("1")], vec![plain("2")]]]);
    }

    #[test]
    fn rule_and_breaks() {
        let lines = parse("above\n\n---\n\nbelow");
        assert!(matches!(lines[1], FormattedTextLine::HorizontalRule));
        // A hard break stays inside one paragraph as an embedded newline.
        let lines = parse("one  \ntwo");
        let FormattedTextLine::Line(inline) = &lines[0] else { panic!() };
        let text: String = inline.iter().map(|f| f.text.as_str()).collect();
        assert_eq!(text, "one\ntwo");
    }

    #[test]
    fn images_become_blocks_with_alt_text() {
        let lines = parse("![logo](img/logo.png \"The Logo\")\n");
        assert_eq!(
            lines[0],
            FormattedTextLine::Image(FormattedImage {
                alt_text: "logo".into(),
                source: "img/logo.png".into(),
                title: Some("The Logo".into()),
            })
        );
    }

    #[test]
    fn yaml_front_matter_is_skipped() {
        let lines = parse("---\ntitle: hidden\n---\n\n# Visible\n");
        assert_eq!(lines.len(), 1);
        assert!(matches!(&lines[0], FormattedTextLine::Heading(h) if h.heading_size == 1));
    }

    #[test]
    fn code_block_inside_list_item_flushes_the_item_first() {
        let lines = parse("- item\n\n  ```sh\n  ls\n  ```\n");
        assert!(matches!(&lines[0], FormattedTextLine::UnorderedList(_)));
        assert!(matches!(&lines[1], FormattedTextLine::CodeBlock(_)));
    }

    #[test]
    fn adjacent_same_style_text_merges_into_one_fragment() {
        // "a & b" arrives as three Text events (entity in the middle) but is
        // one plain fragment after folding.
        let lines = parse("a &amp; b");
        let FormattedTextLine::Line(inline) = &lines[0] else { panic!() };
        assert_eq!(inline.len(), 1);
        assert_eq!(inline[0].text, "a & b");
    }
}
