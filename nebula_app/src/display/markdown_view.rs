//! Read-only document viewer for a tab: Markdown (rendered), JSON
//! (pretty-printed), and plain text. Owns the document MODEL — file loading,
//! format dispatch, word-wrap layout, the virtual scroll window — plus its
//! rendering, mirroring the `settings`/`side_panel` module split. It never
//! touches PTY or grid state: a doc tab has no pane.
//!
//! ## Layout & virtualization contract
//!
//! Opening a file parses it into flat blocks (`markdown::FormattedTextLine`).
//! `relayout` word-wraps those blocks into [`VisualLine`]s — one entry per
//! ON-SCREEN line, each carrying its own y-offset/height — and only re-runs
//! when the wrap key (content width, cell metrics) changes, e.g. on resize or
//! font-size change. Every line wraps to the content column: nothing ever
//! overflows horizontally.
//!
//! Rendering is windowed (the VSCode model): `visible_range` binary-searches
//! the y-prefix array for the slice covering the viewport plus a few buffer
//! lines, and ONLY that slice generates quads/glyphs. A 100k-line document
//! costs a binary search plus ~60 lines of draw work per frame.

use std::ops::Range;
use std::path::{Path, PathBuf};

use nebula_terminal::term::cell::Flags;
use unicode_width::UnicodeWidthChar;

use crate::markdown::{
    self, FormattedTextFragment, FormattedTextInline, FormattedTextLine, TableAlignment,
};
use crate::renderer::ui::UiQuad;
use crate::renderer::{GlyphCache, Renderer};

use super::SizeInfo;
use super::theme::Skin;

/// Extensions the viewer opens (double-click in the file tree). Everything
/// else keeps the tree's default activation behaviour.
pub fn viewable_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("md" | "markdown" | "json" | "jsonl" | "ndjson" | "txt" | "log")
    )
}

// ---- layout constants (logical px, multiplied by scale at use sites) ----

/// Reading column: capped like a Typora page so long lines stay readable on
/// wide windows; centered in the pane area with a comfortable gutter.
const MAX_COLUMN_W: f32 = 860.0;
const GUTTER: f32 = 44.0;
/// Extra visual lines laid out above/below the viewport (the user-visible
/// "load a bit around the window" buffer).
const OVERSCAN_LINES: usize = 8;
/// Body line height, in cell heights.
const BODY_LINE: f32 = 1.55;
const CODE_LINE: f32 = 1.4;
/// Per-level indent of lists and quotes, in px.
const LIST_INDENT: f32 = 26.0;
const QUOTE_INDENT: f32 = 20.0;

/// One wrapped on-screen line, positioned in document space.
struct VisualLine {
    /// Top of the line, in px from the document top.
    y: f32,
    h: f32,
    /// Left inset from the content column, in px.
    indent: f32,
    /// Chrome-font size multiplier (headings > 1).
    scale: f32,
    spans: Vec<Span>,
    decor: Decor,
}

/// Style-resolved run of text within one visual line.
struct Span {
    text: String,
    cols: usize,
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    link: Option<String>,
    /// Dim ink (list bullets, image captions).
    faint: bool,
    /// Source fragment index, so wrapping can regroup chars back into spans
    /// (`usize::MAX` for synthesized labels like bullets and padding).
    frag_key: usize,
}

/// Non-text decoration of a visual line.
#[derive(Clone, Copy, Default)]
struct Decor {
    /// Code-block band behind the line (first/last extend the pad and round
    /// the corners).
    code: bool,
    code_first: bool,
    code_last: bool,
    /// `>` quote bars to the line's left.
    quote_depth: usize,
    /// Horizontal rule: no text, one hairline.
    rule: bool,
    /// Hairline under this line (table header, H1/H2 underline).
    underline_row: bool,
}

/// A document opened in a tab.
pub struct DocView {
    pub path: PathBuf,
    /// Tab label: the file name.
    pub title: String,
    blocks: Vec<FormattedTextLine>,
    visual: Vec<VisualLine>,
    /// (content_w, cell_w×64, cell_h×64) the current layout was built for.
    wrap_key: (u32, u32, u32),
    /// Pixel scroll offset from the document top.
    pub scroll: f32,
    content_h: f32,
}

impl DocView {
    /// Load `path` and build the document model. Never fails: read or parse
    /// problems become a document that shows the error.
    pub fn open(path: PathBuf) -> Self {
        let title = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let blocks = match std::fs::read_to_string(&path) {
            Ok(raw) => blocks_for(&path, &raw),
            Err(err) => vec![FormattedTextLine::Line(vec![FormattedTextFragment::plain_text(
                format!("无法读取 {}: {err}", path.display()),
            )])],
        };
        Self {
            path,
            title,
            blocks,
            visual: Vec::new(),
            wrap_key: (0, 0, 0),
            scroll: 0.0,
            content_h: 0.0,
        }
    }

    /// Re-read the file from disk, keeping the scroll position clamped.
    pub fn reload(&mut self) {
        let path = self.path.clone();
        let scroll = self.scroll;
        *self = Self::open(path);
        self.scroll = scroll;
    }

    /// Total laid-out height, for scroll clamping.
    pub fn max_scroll(&self, viewport_h: f32) -> f32 {
        (self.content_h - viewport_h).max(0.0)
    }

    pub fn scroll_by(&mut self, dy: f32, viewport_h: f32) {
        self.scroll = (self.scroll + dy).clamp(0.0, self.max_scroll(viewport_h));
    }

    /// Word-wrap the blocks for `content_w` at the current font metrics.
    /// Cheap when nothing changed (single key comparison). `adv_w` is the
    /// UNfloored design advance per column — scaled text steps by it, so its
    /// wrap widths must be measured with it too (`cell_w` under-measures by
    /// the floor loss, compounding per column).
    fn relayout(&mut self, content_w: f32, cell_w: f32, cell_h: f32, adv_w: f32, scale: f32) {
        let key = (content_w as u32, (cell_w * 64.0) as u32, (cell_h * 64.0) as u32);
        if key == self.wrap_key && !self.visual.is_empty() {
            return;
        }
        self.wrap_key = key;
        self.visual.clear();

        let mut layout = LayoutCtx {
            out: &mut self.visual,
            y: cell_h * 0.4,
            content_w,
            cell_w,
            cell_h,
            adv_w,
            scale,
        };
        for block in &self.blocks {
            layout.block(block, 0, 0.0);
        }
        self.content_h = layout.y + cell_h * 2.0;
    }

    /// Visual lines overlapping the viewport, padded by the overscan buffer —
    /// the ONLY slice the renderer walks.
    fn visible_range(&self, viewport_h: f32) -> Range<usize> {
        if self.visual.is_empty() {
            return 0..0;
        }
        let top = self.scroll;
        let bottom = self.scroll + viewport_h;
        // First line whose bottom edge reaches the viewport top.
        let start = self.visual.partition_point(|line| line.y + line.h < top);
        let mut end = start;
        while end < self.visual.len() && self.visual[end].y <= bottom {
            end += 1;
        }
        start.saturating_sub(OVERSCAN_LINES)..(end + OVERSCAN_LINES).min(self.visual.len())
    }
}

/// Parse `raw` according to the file extension.
fn blocks_for(path: &Path, raw: &str) -> Vec<FormattedTextLine> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "md" | "markdown" => markdown::parse_markdown(raw).lines.into_iter().collect(),
        "json" => {
            // Pretty-print when it parses; the raw text is still shown as a
            // code block when it doesn't (viewer, not validator).
            let pretty = serde_json::from_str::<serde_json::Value>(raw)
                .and_then(|value| serde_json::to_string_pretty(&value))
                .unwrap_or_else(|_| raw.to_owned());
            vec![code_block("json", pretty)]
        },
        // One JSON value per line: keep the line structure, no prettifying.
        "jsonl" | "ndjson" => vec![code_block("json", raw.to_owned())],
        // Plain text (txt/log/anything readable): body font, blank lines
        // become paragraph gaps.
        _ => raw
            .lines()
            .map(|line| {
                if line.trim().is_empty() {
                    FormattedTextLine::LineBreak
                } else {
                    FormattedTextLine::Line(vec![FormattedTextFragment::plain_text(line)])
                }
            })
            .collect(),
    }
}

fn code_block(lang: &str, code: String) -> FormattedTextLine {
    FormattedTextLine::CodeBlock(crate::markdown::CodeBlockText { lang: lang.to_owned(), code })
}

// ---- wrapping ----

/// Character stream item during wrapping: a char plus the index of the
/// fragment its styles come from.
struct WrapChar {
    ch: char,
    frag: usize,
    cols: usize,
}

/// Wrap `inline` to `max_cols` columns. Returns one span list per visual
/// line. Break points: after spaces, between CJK characters, and at embedded
/// `\n` (hard breaks); a single unbreakable run longer than the line hard-cuts.
fn wrap_inline(inline: &FormattedTextInline, max_cols: usize) -> Vec<Vec<Span>> {
    let max_cols = max_cols.max(4);
    let stream: Vec<WrapChar> = inline
        .iter()
        .enumerate()
        .flat_map(|(frag, fragment)| {
            fragment.text.chars().map(move |ch| WrapChar {
                ch,
                frag,
                cols: ch.width().unwrap_or(0).max(1),
            })
        })
        .collect();

    let mut lines = Vec::new();
    let mut line_start = 0usize;
    let mut cols = 0usize;
    let mut last_break: Option<usize> = None; // stream index AFTER which we may break
    let mut i = 0usize;
    while i < stream.len() {
        let item = &stream[i];
        if item.ch == '\n' {
            lines.push(spans_of(inline, &stream[line_start..i]));
            line_start = i + 1;
            cols = 0;
            last_break = None;
            i += 1;
            continue;
        }
        if cols + item.cols > max_cols && i > line_start {
            // Prefer the last soft break point; hard-cut when the whole line
            // is one unbreakable run.
            let cut = match last_break {
                Some(b) if b > line_start => b,
                _ => i,
            };
            lines.push(spans_of(inline, &stream[line_start..cut]));
            // Swallow the spaces the line was broken at.
            let mut next = cut;
            while next < stream.len() && stream[next].ch == ' ' {
                next += 1;
            }
            line_start = next;
            cols = stream[line_start..i.max(line_start)].iter().map(|c| c.cols).sum();
            last_break = None;
            // `i` stays: the current char re-checks against the fresh line.
            continue;
        }
        cols += item.cols;
        if item.ch == ' ' {
            last_break = Some(i + 1);
        } else if item.cols > 1 {
            // CJK/fullwidth: breakable after the character.
            last_break = Some(i + 1);
        }
        i += 1;
    }
    lines.push(spans_of(inline, &stream[line_start..]));
    lines
}

/// Regroup a wrapped char slice back into style spans. Trailing spaces are
/// dropped (they carry no width at a break point); leading whitespace stays —
/// code lines depend on their indentation.
fn spans_of(inline: &FormattedTextInline, chars: &[WrapChar]) -> Vec<Span> {
    let end = chars.iter().rposition(|c| c.ch != ' ').map_or(0, |p| p + 1);
    let mut spans: Vec<Span> = Vec::new();
    for item in &chars[..end] {
        let need_new = match spans.last() {
            Some(last) => last.frag_key != item.frag,
            None => true,
        };
        if need_new {
            spans.push(Span::from_fragment(&inline[item.frag], item.frag));
        }
        let span = spans.last_mut().unwrap();
        span.text.push(item.ch);
        span.cols += item.cols;
    }
    spans
}

impl Span {
    fn from_fragment(fragment: &FormattedTextFragment, frag_key: usize) -> Self {
        let styles = &fragment.styles;
        Span {
            text: String::new(),
            cols: 0,
            bold: styles.weight.is_some_and(|weight| weight.is_at_least_bold()),
            italic: styles.italic,
            strike: styles.strikethrough,
            code: styles.inline_code,
            link: styles.hyperlink.as_ref().map(|crate::markdown::Hyperlink::Url(url)| url.clone()),
            faint: false,
            frag_key,
        }
    }

    fn label(text: impl Into<String>, faint: bool) -> Self {
        let text = text.into();
        let cols = text.chars().map(|c| c.width().unwrap_or(0).max(1)).sum();
        Span {
            text,
            cols,
            bold: false,
            italic: false,
            strike: false,
            code: false,
            link: None,
            faint,
            frag_key: usize::MAX,
        }
    }
}

// ---- block → visual lines ----

struct LayoutCtx<'a> {
    out: &'a mut Vec<VisualLine>,
    y: f32,
    content_w: f32,
    cell_w: f32,
    cell_h: f32,
    /// Unfloored design advance per column (`metrics.average_advance`); the
    /// step scaled text is drawn with, hence measured with.
    adv_w: f32,
    scale: f32,
}

impl LayoutCtx<'_> {
    fn s(&self, v: f32) -> f32 {
        v * self.scale
    }

    fn push(&mut self, h: f32, indent: f32, text_scale: f32, spans: Vec<Span>, decor: Decor) {
        self.out.push(VisualLine { y: self.y, h, indent, scale: text_scale, spans, decor });
        self.y += h;
    }

    /// Lay out one block at `quote_depth` with `extra_indent` px.
    fn block(&mut self, block: &FormattedTextLine, quote_depth: usize, extra_indent: f32) {
        let ch = self.cell_h;
        let quote_pad = quote_depth as f32 * self.s(QUOTE_INDENT);
        let indent = extra_indent + quote_pad;
        let decor = Decor { quote_depth, ..Decor::default() };
        let cols_at = |indent_px: f32, text_scale: f32| -> usize {
            // Scaled runs advance by the true design step, 1.0× by the grid
            // cell — mirror the draw-side stepping exactly or headings wrap
            // a few percent too late and overflow the column.
            let step = if text_scale == 1.0 { self.cell_w } else { self.adv_w * text_scale };
            ((self.content_w - indent_px) / step).max(4.0) as usize
        };

        match block {
            FormattedTextLine::Heading(header) => {
                let (text_scale, top, bottom) = match header.heading_size {
                    // Sharply separated tiers (SiYuan/Typora-like): H1 reads as
                    // a page title, H2 as a section, H3 a step below body+bold.
                    1 => (1.7, 1.1, 0.45),
                    2 => (1.42, 0.95, 0.4),
                    3 => (1.22, 0.8, 0.3),
                    _ => (1.08, 0.7, 0.25),
                };
                self.y += ch * top;
                let wrapped = wrap_inline(&header.text, cols_at(indent, text_scale));
                let count = wrapped.len();
                for (i, mut spans) in wrapped.into_iter().enumerate() {
                    for span in &mut spans {
                        span.bold = true;
                    }
                    let mut decor = decor;
                    // Typora-style underline on H1/H2, on the last wrap line.
                    decor.underline_row = header.heading_size <= 2 && i + 1 == count;
                    self.push(ch * text_scale * 1.45, indent, text_scale, spans, decor);
                }
                self.y += ch * bottom;
            },
            FormattedTextLine::Line(inline) => {
                for spans in wrap_inline(inline, cols_at(indent, 1.0)) {
                    self.push(ch * BODY_LINE, indent, 1.0, spans, decor);
                }
                self.y += ch * 0.5;
            },
            FormattedTextLine::UnorderedList(list) => {
                self.list_item(&list.text, list.indent_level, "•  ", quote_depth, extra_indent);
            },
            FormattedTextLine::OrderedList(list) => {
                let marker = match list.number {
                    Some(n) => format!("{n}. "),
                    None => "•  ".to_owned(),
                };
                self.list_item(
                    &list.indented_text.text,
                    list.indented_text.indent_level,
                    &marker,
                    quote_depth,
                    extra_indent,
                );
            },
            FormattedTextLine::TaskList(task) => {
                let marker = if task.complete { "☑ " } else { "☐ " };
                self.list_item(&task.text, task.indent_level, marker, quote_depth, extra_indent);
            },
            FormattedTextLine::CodeBlock(code) => {
                self.y += ch * 0.4;
                let code_cols = cols_at(indent + self.s(28.0), 1.0);
                let source_lines: Vec<&str> =
                    if code.code.is_empty() { vec![""] } else { code.code.lines().collect() };
                let mut rows: Vec<Vec<Span>> = Vec::new();
                for source in source_lines {
                    let inline = vec![FormattedTextFragment::plain_text(source)];
                    for mut spans in wrap_inline(&inline, code_cols) {
                        for span in &mut spans {
                            span.code = true;
                        }
                        rows.push(spans);
                    }
                }
                let count = rows.len();
                for (i, spans) in rows.into_iter().enumerate() {
                    let mut decor = decor;
                    decor.code = true;
                    decor.code_first = i == 0;
                    decor.code_last = i + 1 == count;
                    self.push(ch * CODE_LINE, indent + self.s(14.0), 1.0, spans, decor);
                }
                self.y += ch * 0.6;
            },
            FormattedTextLine::Quote { depth, line } => {
                self.block(line, *depth, extra_indent);
            },
            FormattedTextLine::LineBreak => {
                self.y += ch * 0.65;
            },
            FormattedTextLine::HorizontalRule => {
                self.y += ch * 0.5;
                let mut decor = decor;
                decor.rule = true;
                self.push(ch * 0.6, indent, 1.0, Vec::new(), decor);
                self.y += ch * 0.5;
            },
            FormattedTextLine::Image(image) => {
                let caption = if image.alt_text.is_empty() { "图片" } else { &image.alt_text };
                let mut span = Span::label(format!("🖼 {caption} — {}", image.source), true);
                span.italic = true;
                span.link = Some(image.source.clone());
                self.push(ch * BODY_LINE, indent, 1.0, vec![span], decor);
                self.y += ch * 0.4;
            },
            FormattedTextLine::Table(table) => {
                self.table(table, quote_depth, indent);
            },
        }
    }

    fn list_item(
        &mut self,
        text: &FormattedTextInline,
        level: usize,
        marker: &str,
        quote_depth: usize,
        extra_indent: f32,
    ) {
        let ch = self.cell_h;
        let quote_pad = quote_depth as f32 * self.s(QUOTE_INDENT);
        let indent = extra_indent + quote_pad + level as f32 * self.s(LIST_INDENT);
        let marker_cols: usize = marker.chars().map(|c| c.width().unwrap_or(0).max(1)).sum();
        let marker_px = marker_cols as f32 * self.cell_w;
        let cols = ((self.content_w - indent - marker_px) / self.cell_w).max(4.0) as usize;
        let decor = Decor { quote_depth, ..Decor::default() };
        for (i, mut spans) in wrap_inline(text, cols).into_iter().enumerate() {
            let line_indent = if i == 0 {
                spans.insert(0, Span::label(marker, true));
                indent
            } else {
                // Hanging indent: wrap lines align under the text, not the
                // marker.
                indent + marker_px
            };
            self.push(ch * BODY_LINE * 0.98, line_indent, 1.0, spans, decor);
        }
        self.y += ch * 0.18;
    }

    /// Fixed-grid table: columns sized to their widest cell (bounded by an
    /// even split of the available width), cells truncated with `…` — the
    /// one place the viewer truncates instead of wrapping.
    fn table(&mut self, table: &crate::markdown::FormattedTable, quote_depth: usize, indent: f32) {
        let ch = self.cell_h;
        let inline_cols = |inline: &FormattedTextInline| -> usize {
            inline.iter().flat_map(|f| f.text.chars()).map(|c| c.width().unwrap_or(0).max(1)).sum()
        };
        let col_count =
            table.headers.len().max(table.rows.iter().map(Vec::len).max().unwrap_or(0)).max(1);
        let total_cols = ((self.content_w - indent) / self.cell_w) as usize;
        // 3 columns of separator (" │ ") between cells.
        let usable = total_cols.saturating_sub((col_count - 1) * 3).max(col_count * 4);
        let fair = usable / col_count;
        let mut widths = vec![0usize; col_count];
        for (i, cell) in table.headers.iter().enumerate() {
            widths[i] = widths[i].max(inline_cols(cell));
        }
        for row in &table.rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(inline_cols(cell));
            }
        }
        for width in &mut widths {
            *width = (*width).clamp(3, fair.max(4));
        }

        let decor = Decor { quote_depth, ..Decor::default() };
        self.y += ch * 0.35;
        let empty_row: Vec<FormattedTextInline> = Vec::new();
        let rows = std::iter::once((&table.headers, true))
            .chain(table.rows.iter().map(|row| (row, false)))
            .map(|(row, is_head)| (if row.is_empty() { &empty_row } else { row }, is_head));
        for (row, is_head) in rows {
            let mut spans: Vec<Span> = Vec::new();
            for (i, width) in widths.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::label(" │ ", true));
                }
                let alignment = table.alignments.get(i).copied().unwrap_or(TableAlignment::Left);
                for mut span in cell_spans(row.get(i), *width, alignment) {
                    span.bold |= is_head;
                    spans.push(span);
                }
            }
            let mut decor = decor;
            decor.underline_row = is_head;
            self.push(ch * BODY_LINE * 0.95, indent, 1.0, spans, decor);
        }
        self.y += ch * 0.6;
    }
}

/// One table cell: styled spans truncated/padded to exactly `width` columns.
fn cell_spans(
    cell: Option<&FormattedTextInline>,
    width: usize,
    alignment: TableAlignment,
) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut used = 0usize;
    if let Some(inline) = cell {
        'outer: for (frag, fragment) in inline.iter().enumerate() {
            for ch in fragment.text.chars() {
                let cols = ch.width().unwrap_or(0).max(1);
                if used + cols > width {
                    // No room left: truncate with an ellipsis if one fits.
                    if used < width {
                        spans.push(Span::label("…", true));
                        used += 1;
                    }
                    break 'outer;
                }
                let need_new = match spans.last() {
                    Some(last) => last.frag_key != frag,
                    None => true,
                };
                if need_new {
                    spans.push(Span::from_fragment(&inline[frag], frag));
                }
                let span = spans.last_mut().unwrap();
                span.text.push(ch);
                span.cols += cols;
                used += cols;
            }
        }
    }
    if used < width {
        let pad = " ".repeat(width - used);
        match alignment {
            TableAlignment::Left => spans.push(Span::label(pad, true)),
            TableAlignment::Right => spans.insert(0, Span::label(pad, true)),
            TableAlignment::Center => {
                let half = (width - used) / 2;
                spans.insert(0, Span::label(" ".repeat(half), true));
                spans.push(Span::label(" ".repeat(width - used - half), true));
            },
        }
    }
    spans
}

// ---- rendering ----

/// Content-column rect inside the pane area `(x, y, w, h)`: reading width cap
/// + centering.
fn column_rect(area: (f32, f32, f32, f32), scale: f32) -> (f32, f32, f32, f32) {
    let (ax, ay, aw, ah) = area;
    let gutter = GUTTER * scale;
    let w = (aw - 2.0 * gutter).min(MAX_COLUMN_W * scale).max(120.0 * scale);
    let x = ax + ((aw - w) / 2.0).max(gutter);
    (x, ay + 8.0 * scale, w, ah - 16.0 * scale)
}

/// Draw the whole document view for this frame: background decor quads, then
/// the visible lines' text, then the overlay scrollbar. `area` is the pane
/// content region (chrome-exclusive), screen-space.
pub fn draw(
    doc: &mut DocView,
    renderer: &mut Renderer,
    glyph_cache: &mut GlyphCache,
    size: &SizeInfo,
    skin: &Skin,
    area: (f32, f32, f32, f32),
    scale: f32,
) {
    let s = |v: f32| v * scale;
    let cell_w = size.cell_width();
    let cell_h = size.cell_height();
    // Unfloored design advance: what scaled text steps by (see relayout).
    let adv_base = glyph_cache.font_metrics().average_advance as f32;
    let (cx, cy, cw, chh) = column_rect(area, scale);
    // Glyphs must land on whole physical pixels: the layout accumulates
    // fractional heights (1.55 line heights, centered columns), and drawing
    // at sub-pixel offsets bilinear-samples every glyph into a blur.
    let (cx, cy) = (cx.round(), cy.round());

    doc.relayout(cw, cell_w, cell_h, adv_base, scale);
    doc.scroll = doc.scroll.clamp(0.0, doc.max_scroll(chh));

    let range = doc.visible_range(chh);
    let clip_top = area.1;
    let clip_bot = area.1 + area.3;

    // Pass 1: decoration quads (code bands, quote bars, rules, underlines,
    // link underlines, strikethrough) — batched, clipped to the pane area.
    let mut quads: Vec<UiQuad> = Vec::new();
    let clip = |quads: &mut Vec<UiQuad>, quad: UiQuad| {
        if let Some(quad) = quad.clip_y(clip_top, clip_bot) {
            quads.push(quad);
        }
    };
    for line in &doc.visual[range.clone()] {
        let y = (cy + line.y - doc.scroll).round();
        let decor = line.decor;
        if decor.code {
            let pad_top = if decor.code_first { s(8.0) } else { 0.0 };
            let pad_bot = if decor.code_last { s(8.0) } else { 0.0 };
            clip(
                &mut quads,
                UiQuad::solid(
                    cx + line.indent - s(14.0),
                    y - pad_top,
                    cw - line.indent + s(14.0),
                    line.h + pad_top + pad_bot,
                    if decor.code_first || decor.code_last { s(6.0) } else { 0.0 },
                    skin.surface,
                ),
            );
        }
        for depth in 0..decor.quote_depth {
            clip(
                &mut quads,
                UiQuad::solid(
                    cx + depth as f32 * s(QUOTE_INDENT),
                    y,
                    s(3.0),
                    line.h,
                    s(1.5),
                    skin.accent_soft,
                ),
            );
        }
        if decor.rule {
            clip(
                &mut quads,
                UiQuad::solid(
                    cx + line.indent,
                    y + line.h / 2.0,
                    cw - line.indent,
                    s(1.0),
                    0.0,
                    skin.hairline,
                ),
            );
        }
        if decor.underline_row {
            clip(
                &mut quads,
                UiQuad::solid(
                    cx + line.indent,
                    y + line.h - s(2.0),
                    cw - line.indent,
                    s(1.0),
                    0.0,
                    skin.hairline,
                ),
            );
        }
        // Span-level decor rides the text advance. Same rounding as the text
        // pass below, so underlines sit exactly under their glyphs; same
        // per-column step too (grid cell at 1.0×, true design advance when
        // scaled — the widths the glyphs are actually drawn at).
        let text_y = (y + (line.h - cell_h * line.scale) / 2.0).round();
        let span_adv = if line.scale == 1.0 { cell_w } else { adv_base * line.scale };
        let mut pen_x = cx + line.indent;
        for span in &line.spans {
            let w = span.cols as f32 * span_adv;
            let px = pen_x.round();
            if span.code && !decor.code {
                // Inline code chip.
                clip(
                    &mut quads,
                    UiQuad::solid(
                        px - s(2.0),
                        text_y - s(2.0),
                        w + s(4.0),
                        cell_h * line.scale + s(4.0),
                        s(4.0),
                        skin.surface,
                    ),
                );
            }
            if span.link.is_some() {
                clip(
                    &mut quads,
                    UiQuad::solid(
                        px,
                        text_y + cell_h * line.scale + s(1.0),
                        w,
                        s(1.0),
                        0.0,
                        skin.accent_soft,
                    ),
                );
            }
            if span.strike {
                clip(
                    &mut quads,
                    UiQuad::solid(
                        px,
                        text_y + cell_h * line.scale * 0.55,
                        w,
                        s(1.0),
                        0.0,
                        skin.hairline,
                    ),
                );
            }
            pen_x += w;
        }
    }

    // Overlay scrollbar (thin thumb, no track — same language as the panes).
    if doc.content_h > chh {
        let track_h = chh - s(12.0);
        let thumb_h = (track_h * chh / doc.content_h).max(s(28.0));
        let frac = doc.scroll / doc.max_scroll(chh);
        let ty = cy + s(6.0) + (track_h - thumb_h) * frac;
        quads.push(UiQuad::solid(
            area.0 + area.2 - s(7.0),
            ty,
            s(4.0),
            thumb_h,
            s(2.0),
            skin.scrollbar_thumb.with_alpha(0.45),
        ));
    }
    renderer.draw_ui(size, &quads);

    // Pass 2: text. A glyph never crosses the pane's top/bottom edge — lines
    // fully outside were never produced (virtual window), partially clipped
    // edge lines are skipped like the settings modal does. Anchors are rounded
    // with EXACTLY the same expressions as pass 1, so chips/underlines sit
    // pixel-true under their glyphs; headings go through `draw_doc_text`,
    // which rasterizes at the real scaled font size instead of stretching
    // base-size atlas bitmaps (the old fuzzy-edge artifact).
    for line in &doc.visual[range] {
        let y = (cy + line.y - doc.scroll).round();
        let text_y = (y + (line.h - cell_h * line.scale) / 2.0).round();
        if text_y < clip_top || text_y + cell_h * line.scale > clip_bot {
            continue;
        }
        let mut pen_x = cx + line.indent;
        for span in &line.spans {
            let ink = if span.link.is_some() {
                skin.accent
            } else if span.code {
                skin.ink_strong
            } else if span.faint {
                skin.ink_faint
            } else if span.bold {
                skin.ink_strong
            } else if span.italic {
                skin.ink_dim
            } else {
                skin.ink
            };
            // Emphasis is carried by the actual face, not just ink.
            let mut style = Flags::empty();
            if span.bold {
                style |= Flags::BOLD;
            }
            if span.italic {
                style |= Flags::ITALIC;
            }
            renderer.draw_doc_text(
                size,
                pen_x.round(),
                text_y,
                line.scale,
                ink,
                style,
                &span.text,
                glyph_cache,
            );
            let span_adv = if line.scale == 1.0 { cell_w } else { adv_base * line.scale };
            pen_x += span.cols as f32 * span_adv;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans_text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn wrap_breaks_at_spaces_and_respects_width() {
        let inline = vec![FormattedTextFragment::plain_text("alpha beta gamma delta")];
        let lines = wrap_inline(&inline, 11);
        let text: Vec<String> = lines.iter().map(|l| spans_text(l)).collect();
        assert_eq!(text, vec!["alpha beta", "gamma delta"]);
    }

    #[test]
    fn wrap_hard_cuts_unbreakable_runs() {
        let inline = vec![FormattedTextFragment::plain_text("aaaaaaaaaaaa")];
        let lines = wrap_inline(&inline, 5);
        let text: Vec<String> = lines.iter().map(|l| spans_text(l)).collect();
        assert_eq!(text, vec!["aaaaa", "aaaaa", "aa"]);
    }

    #[test]
    fn wrap_breaks_cjk_anywhere_by_column_width() {
        // 6 CJK chars = 12 columns; a 8-column line fits 4 chars.
        let inline = vec![FormattedTextFragment::plain_text("终端里的中文")];
        let lines = wrap_inline(&inline, 8);
        let text: Vec<String> = lines.iter().map(|l| spans_text(l)).collect();
        assert_eq!(text, vec!["终端里的", "中文"]);
    }

    #[test]
    fn wrap_honours_embedded_hard_breaks() {
        let inline = vec![FormattedTextFragment::plain_text("one\ntwo")];
        let lines = wrap_inline(&inline, 40);
        let text: Vec<String> = lines.iter().map(|l| spans_text(l)).collect();
        assert_eq!(text, vec!["one", "two"]);
    }

    #[test]
    fn wrap_preserves_styles_across_lines() {
        let inline = vec![
            FormattedTextFragment::plain_text("plain "),
            FormattedTextFragment::bold("bold text that wraps"),
        ];
        let lines = wrap_inline(&inline, 12);
        assert!(lines.len() >= 2);
        // The bold fragment stays bold on every wrapped line it spans.
        for line in &lines[1..] {
            assert!(line.iter().all(|span| span.bold));
        }
    }

    #[test]
    fn visual_layout_is_virtualizable() {
        let markdown = (0..500).map(|i| format!("paragraph {i}\n\n")).collect::<String>();
        let mut doc = DocView {
            path: PathBuf::from("test.md"),
            title: "test.md".into(),
            blocks: crate::markdown::parse_markdown(&markdown).lines.into_iter().collect(),
            visual: Vec::new(),
            wrap_key: (0, 0, 0),
            scroll: 0.0,
            content_h: 0.0,
        };
        doc.relayout(600.0, 8.0, 16.0, 8.0, 1.0);
        assert_eq!(doc.visual.len(), 500);
        // y offsets are strictly increasing (binary search precondition).
        assert!(doc.visual.windows(2).all(|w| w[0].y < w[1].y));

        // A mid-document viewport touches only a small window of lines.
        doc.scroll = doc.content_h / 2.0;
        let range = doc.visible_range(400.0);
        assert!(range.len() < 40, "viewport window too large: {range:?}");
        assert!(range.start > 100, "window did not move with scroll");
    }

    #[test]
    fn json_files_pretty_print_into_a_code_block() {
        let blocks = blocks_for(Path::new("x.json"), r#"{"b":1,"a":[1,2]}"#);
        let [FormattedTextLine::CodeBlock(code)] = &blocks[..] else {
            panic!("expected one code block, got {blocks:?}");
        };
        assert_eq!(code.lang, "json");
        assert!(code.code.contains("\n"), "should be prettified: {}", code.code);
    }

    #[test]
    fn plain_text_lines_become_paragraphs() {
        let blocks = blocks_for(Path::new("notes.txt"), "first\n\nsecond");
        assert!(matches!(&blocks[0], FormattedTextLine::Line(_)));
        assert!(matches!(&blocks[1], FormattedTextLine::LineBreak));
        assert!(matches!(&blocks[2], FormattedTextLine::Line(_)));
    }
}
