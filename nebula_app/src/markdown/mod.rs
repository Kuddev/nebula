//! Self-contained Markdown document model. This module owns ONLY parsing and
//! the parsed representation — no rendering, no Display state, no UI types.
//! The viewer that draws these values lives in `display::markdown_view`.
//!
//! The shape is a FLAT sequence of block-level lines (a paragraph is one
//! entry, a whole fenced code block is one entry, …) rather than a nested
//! tree: the viewer's virtual scroller needs to binary-search "which block is
//! at pixel offset Y" and lay out only the visible slice, which a flat list
//! gives for free. Nesting that survives (list levels, quote depth) is
//! carried as data on the block, not as tree structure.

use std::collections::VecDeque;
use std::fmt;
use std::ops::Range;

pub mod parser;

pub use parser::parse_markdown;

/// Font weight carried by a fragment. Only `Bold` is ever produced by the
/// Markdown parser; the full scale exists so the renderer maps weights, not
/// booleans.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CustomWeight {
    Thin,
    ExtraLight,
    Light,
    Medium,
    Semibold,
    Bold,
    ExtraBold,
    Black,
}

impl CustomWeight {
    /// Returns true if the weight is bold or heavier.
    pub fn is_at_least_bold(&self) -> bool {
        matches!(self, CustomWeight::Bold | CustomWeight::ExtraBold | CustomWeight::Black)
    }
}

/// A parsed Markdown document: a flat sequence of block-level lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FormattedText {
    pub lines: VecDeque<FormattedTextLine>,
}

impl FormattedText {
    pub fn new(lines: impl Into<VecDeque<FormattedTextLine>>) -> Self {
        Self { lines: lines.into() }
    }

    /// The document's raw text, without any of the markdown markers.
    pub fn raw_text(&self) -> String {
        self.lines.iter().map(|line| line.raw_text()).collect()
    }
}

/// One block-level element of the document.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum FormattedTextLine {
    Heading(FormattedTextHeader),
    Line(FormattedTextInline),
    OrderedList(OrderedFormattedIndentTextInline),
    UnorderedList(FormattedIndentTextInline),
    CodeBlock(CodeBlockText),
    TaskList(FormattedTaskList),
    LineBreak,
    HorizontalRule,
    Image(FormattedImage),
    Table(FormattedTable),
    /// A block nested inside `depth` levels of `>` quoting. Quoting wraps the
    /// inner block instead of forking every variant, so the flat list (and
    /// the virtual scroller's block indexing) survives.
    Quote { depth: usize, line: Box<FormattedTextLine> },
}

impl FormattedTextLine {
    pub fn raw_text(&self) -> String {
        let join = |inline: &FormattedTextInline| -> String {
            inline.iter().map(|fragment| fragment.text.as_str()).collect()
        };
        let mut text = match self {
            Self::CodeBlock(text) => text.code.clone(),
            Self::Heading(header) => join(&header.text),
            Self::Line(line) => join(line),
            Self::TaskList(line) => join(&line.text),
            Self::OrderedList(list) => join(&list.indented_text.text),
            Self::UnorderedList(list) => join(&list.text),
            Self::LineBreak | Self::HorizontalRule => "\n".to_string(),
            Self::Image(image) => format!("{}\n", image.alt_text),
            Self::Table(table) => table.to_plain_text(),
            Self::Quote { line, .. } => line.raw_text(),
        };
        // Each `FormattedTextLine` unit represents a complete line.
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text
    }

    fn inline_fragments(&self) -> Option<&FormattedTextInline> {
        match &self {
            FormattedTextLine::Heading(header) => Some(&header.text),
            FormattedTextLine::Line(texts) => Some(texts),
            FormattedTextLine::OrderedList(texts) => Some(&texts.indented_text.text),
            FormattedTextLine::UnorderedList(texts) => Some(&texts.text),
            FormattedTextLine::TaskList(list) => Some(&list.text),
            FormattedTextLine::Quote { line, .. } => line.inline_fragments(),
            FormattedTextLine::CodeBlock(_)
            | FormattedTextLine::LineBreak
            | FormattedTextLine::HorizontalRule
            | FormattedTextLine::Image(_)
            | FormattedTextLine::Table(_) => None,
        }
    }

    /// Hyperlinks of this line as (char range within the concatenated inline
    /// text, url) pairs — the viewer maps clicks through this.
    pub fn hyperlinks(&self) -> Vec<(Range<usize>, String)> {
        let mut hyperlinks = Vec::new();
        if let Some(inline_fragments) = self.inline_fragments() {
            let mut char_count = 0;
            for fragment in inline_fragments {
                let range_start = char_count;
                char_count += fragment.text.chars().count();
                if let Some(Hyperlink::Url(url)) = &fragment.styles.hyperlink {
                    hyperlinks.push((range_start..char_count, url.clone()));
                }
            }
        }
        hyperlinks
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FormattedTextHeader {
    pub heading_size: usize,
    pub text: FormattedTextInline,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FormattedTaskList {
    pub complete: bool,
    pub indent_level: usize,
    pub text: FormattedTextInline,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FormattedIndentTextInline {
    pub indent_level: usize,
    pub text: FormattedTextInline,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CodeBlockText {
    pub lang: String,
    pub code: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct OrderedFormattedIndentTextInline {
    /// The number of this item, which may be `None` if it was unspecified or
    /// invalid in the source document.
    pub number: Option<usize>,
    pub indented_text: FormattedIndentTextInline,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FormattedImage {
    pub alt_text: String,
    pub source: String,
    /// Optional CommonMark image title, e.g. the `title` in `![alt](src "title")`.
    /// Empty titles are normalized to `None` by the parser.
    pub title: Option<String>,
}

/// Column alignment for table cells.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default, Hash)]
pub enum TableAlignment {
    #[default]
    Left,
    Center,
    Right,
}

/// A formatted table with headers, alignments, and rows.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FormattedTable {
    pub headers: Vec<FormattedTextInline>,
    pub alignments: Vec<TableAlignment>,
    pub rows: Vec<Vec<FormattedTextInline>>,
}

impl FormattedTable {
    /// Serialize to GFM pipe-table markdown (used for raw-text extraction).
    pub fn to_plain_text(&self) -> String {
        fn inline_to_text(inline: &FormattedTextInline) -> String {
            inline.iter().map(|f| f.text.as_str()).collect()
        }

        let mut lines = Vec::new();
        let headers: Vec<String> = self.headers.iter().map(inline_to_text).collect();
        lines.push(format!("| {} |", headers.join(" | ")));
        let separator: Vec<String> = self
            .alignments
            .iter()
            .map(|alignment| match alignment {
                TableAlignment::Left => "---".to_string(),
                TableAlignment::Center => ":---:".to_string(),
                TableAlignment::Right => "---:".to_string(),
            })
            .collect();
        lines.push(format!("| {} |", separator.join(" | ")));
        for row in &self.rows {
            let cells: Vec<String> = row.iter().map(inline_to_text).collect();
            lines.push(format!("| {} |", cells.join(" | ")));
        }
        lines.join("\n")
    }
}

pub type FormattedTextInline = Vec<FormattedTextFragment>;

/// A fragment of formatted text: the text itself plus formatting flags.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct FormattedTextFragment {
    pub text: String,
    pub styles: FormattedTextStyles,
}

/// A clickable link target. Kept as an enum so a future in-app action target
/// (e.g. "open this file in a new tab") slots in beside plain URLs.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Hyperlink {
    Url(String),
}

/// Formatted text styling, with no attached content.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct FormattedTextStyles {
    pub weight: Option<CustomWeight>,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub inline_code: bool,
    pub hyperlink: Option<Hyperlink>,
}

impl FormattedTextFragment {
    pub fn plain_text(text: impl Into<String>) -> Self {
        Self { text: text.into(), styles: Default::default() }
    }

    pub fn bold(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            styles: FormattedTextStyles {
                weight: Some(CustomWeight::Bold),
                ..Default::default()
            },
        }
    }

    pub fn italic(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            styles: FormattedTextStyles { italic: true, ..Default::default() },
        }
    }

    pub fn hyperlink(tag: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            text: tag.into(),
            styles: FormattedTextStyles {
                hyperlink: Some(Hyperlink::Url(url.into())),
                ..Default::default()
            },
        }
    }

    pub fn inline_code(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            styles: FormattedTextStyles { inline_code: true, ..Default::default() },
        }
    }
}

impl fmt::Debug for FormattedTextStyles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // For readability, only show active styles.
        let mut parts: Vec<String> = Vec::new();
        if let Some(weight) = self.weight {
            parts.push(format!("{weight:?}"));
        }
        if self.italic {
            parts.push("Italic".into());
        }
        if self.underline {
            parts.push("Underline".into());
        }
        if self.strikethrough {
            parts.push("Strikethrough".into());
        }
        if self.inline_code {
            parts.push("InlineCode".into());
        }
        if let Some(link) = &self.hyperlink {
            parts.push(format!("Hyperlink({link:?})"));
        }
        if parts.is_empty() {
            // No styles are active, so this is plain text.
            f.write_str("PlainText")
        } else {
            f.write_str(&parts.join(" | "))
        }
    }
}
