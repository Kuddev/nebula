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
use std::sync::Arc;

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
    /// Markdown 源码只保留这一份共享字节；fragment 用范围引用它，避免逐层复制。
    pub source: Arc<str>,
    pub lines: VecDeque<FormattedTextLine>,
}

impl FormattedText {
    pub fn new(lines: impl Into<VecDeque<FormattedTextLine>>) -> Self {
        Self { source: Arc::from(""), lines: lines.into() }
    }

    /// The document's raw text, without any of the markdown markers.
    pub fn raw_text(&self) -> String {
        self.lines.iter().map(|line| line.raw_text()).collect()
    }
}

/// UTF-8 源码中的半开字节范围。文档输入上限在解析入口保证可用 `u32` 表示。
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct SourceRange {
    pub start: u32,
    pub end: u32,
}

impl SourceRange {
    pub fn new(range: Range<usize>) -> Option<Self> {
        Some(Self { start: range.start.try_into().ok()?, end: range.end.try_into().ok()? })
    }

    pub fn as_usize(self) -> Range<usize> {
        self.start as usize..self.end as usize
    }
}

/// 绝大多数内容引用 Markdown 源码；只有实体解码、软换行等合成文本单独分配。
#[derive(Clone, Debug)]
pub enum TextRef {
    Source { source: Arc<str>, range: SourceRange },
    Generated(Box<str>),
}

impl Default for TextRef {
    fn default() -> Self {
        Self::Generated(Box::default())
    }
}

impl PartialEq for TextRef {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for TextRef {}

impl TextRef {
    pub fn source(source: Arc<str>, range: Range<usize>) -> Option<Self> {
        let range = SourceRange::new(range)?;
        source.get(range.as_usize())?;
        Some(Self::Source { source, range })
    }

    pub fn generated(text: impl Into<Box<str>>) -> Self {
        Self::Generated(text.into())
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Source { source, range } => &source[range.as_usize()],
            Self::Generated(text) => text,
        }
    }

    /// 只合并同一源码中的连续范围；跨 Markdown 标记的内容保持独立，避免错误扩大范围。
    fn append(&mut self, next: Self) -> Result<(), Self> {
        match (self, next) {
            (
                Self::Source { source, range },
                Self::Source { source: next_source, range: next_range },
            ) if Arc::ptr_eq(source, &next_source) && range.end == next_range.start => {
                range.end = next_range.end;
                Ok(())
            },
            (slot @ Self::Generated(_), Self::Generated(next)) => {
                let mut combined = String::from(slot.as_str());
                combined.push_str(&next);
                *slot = Self::Generated(combined.into_boxed_str());
                Ok(())
            },
            // 实体解码或反斜杠转义会在同样式文本中留下源码间隙；此时只合成这一小段，
            // 既保持旧的 fragment 合并语义，也不扩大 range 到 Markdown 标记字节。
            (slot, next) => {
                let mut combined = String::from(slot.as_str());
                combined.push_str(next.as_str());
                *slot = Self::Generated(combined.into_boxed_str());
                Ok(())
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MathMode {
    Inline,
    Display,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MathSource {
    pub source: TextRef,
    pub mode: MathMode,
}

impl MathSource {
    pub fn as_str(&self) -> &str {
        self.source.as_str()
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
    DisplayMath(MathSource),
    /// A block nested inside `depth` levels of `>` quoting. Quoting wraps the
    /// inner block instead of forking every variant, so the flat list (and
    /// the virtual scroller's block indexing) survives.
    Quote {
        depth: usize,
        line: Box<FormattedTextLine>,
    },
}

impl FormattedTextLine {
    pub fn raw_text(&self) -> String {
        let join = |inline: &FormattedTextInline| -> String {
            inline.iter().map(FormattedTextFragment::text).collect()
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
            Self::DisplayMath(math) => math.as_str().to_owned(),
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
            | FormattedTextLine::Table(_)
            | FormattedTextLine::DisplayMath(_) => None,
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
                char_count += fragment.text().chars().count();
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
            inline.iter().map(FormattedTextFragment::text).collect()
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

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum FragmentContent {
    Text(TextRef),
    Math(MathSource),
}

impl Default for FragmentContent {
    fn default() -> Self {
        Self::Text(TextRef::default())
    }
}

/// A fragment of formatted text or an inline mathematical object plus formatting flags.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct FormattedTextFragment {
    pub content: FragmentContent,
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
        Self::from_generated(text, Default::default())
    }

    pub fn bold(text: impl Into<String>) -> Self {
        Self {
            content: FragmentContent::Text(TextRef::generated(text.into().into_boxed_str())),
            styles: FormattedTextStyles { weight: Some(CustomWeight::Bold), ..Default::default() },
        }
    }

    pub fn italic(text: impl Into<String>) -> Self {
        Self {
            content: FragmentContent::Text(TextRef::generated(text.into().into_boxed_str())),
            styles: FormattedTextStyles { italic: true, ..Default::default() },
        }
    }

    pub fn hyperlink(tag: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            content: FragmentContent::Text(TextRef::generated(tag.into().into_boxed_str())),
            styles: FormattedTextStyles {
                hyperlink: Some(Hyperlink::Url(url.into())),
                ..Default::default()
            },
        }
    }

    pub fn inline_code(text: impl Into<String>) -> Self {
        Self {
            content: FragmentContent::Text(TextRef::generated(text.into().into_boxed_str())),
            styles: FormattedTextStyles { inline_code: true, ..Default::default() },
        }
    }

    pub fn from_text(text: TextRef, styles: FormattedTextStyles) -> Self {
        Self { content: FragmentContent::Text(text), styles }
    }

    pub fn math(source: MathSource, styles: FormattedTextStyles) -> Self {
        Self { content: FragmentContent::Math(source), styles }
    }

    pub fn text(&self) -> &str {
        match &self.content {
            FragmentContent::Text(text) => text.as_str(),
            FragmentContent::Math(math) => math.as_str(),
        }
    }

    pub fn is_math(&self) -> bool {
        matches!(self.content, FragmentContent::Math(_))
    }

    pub(crate) fn append_text(&mut self, text: TextRef) -> Result<(), TextRef> {
        match &mut self.content {
            FragmentContent::Text(current) => current.append(text),
            FragmentContent::Math(_) => Err(text),
        }
    }

    fn from_generated(text: impl Into<String>, styles: FormattedTextStyles) -> Self {
        let text = text.into().into_boxed_str();
        Self { content: FragmentContent::Text(TextRef::generated(text)), styles }
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
