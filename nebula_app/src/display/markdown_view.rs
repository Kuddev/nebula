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
    self, FormattedTextFragment, FormattedTextInline, FormattedTextLine, FragmentContent,
    MathSource, TableAlignment,
};
use crate::math::cache::{FormulaCacheKey, MathLayoutCache};
use crate::math::layout::{MathMetrics, layout_formula};
use crate::math::{DEFAULT_LIMITS, parse_formula};
use crate::renderer::math::MathClip;
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
/// Below this size a fitted formula stops being readable; the existing
/// source-text fallback then wraps it normally instead of clipping tiny ink.
const MIN_FITTED_MATH_PX: f32 = 6.0;
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
    /// 公式行使用显式基线；纯文本保持原来的垂直居中路径。
    math_baseline: Option<f32>,
    center_math: bool,
}

/// Style-resolved run of text within one visual line.
struct Span {
    text: String,
    cols: usize,
    bold: bool,
    /// Use strong theme ink without selecting a synthetic bold font face.
    strong_ink: bool,
    italic: bool,
    strike: bool,
    code: bool,
    link: Option<String>,
    /// Dim ink (list bullets, image captions).
    faint: bool,
    /// Source fragment index, so wrapping can regroup chars back into spans
    /// (`usize::MAX` for synthesized labels like bullets and padding).
    frag_key: usize,
    math: Option<MathRun>,
}

#[derive(Clone)]
struct MathRun {
    source: MathSource,
    formula_id: u64,
    pixel_size: f32,
    pixels_per_point: f32,
    display: bool,
    metrics: MathMetrics,
    advance_width: f32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct WrapKey {
    content_width: u32,
    cell_width: u32,
    cell_height: u32,
    text_advance: u32,
    font_pixel_size: u32,
    pixels_per_point: u32,
    text_ascent: u32,
    scale: u32,
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
    /// 精确覆盖文字与公式度量输入；跨屏 DPI 或字体变化必须触发重排。
    wrap_key: WrapKey,
    /// Pixel scroll offset from the document top.
    pub scroll: f32,
    content_h: f32,
    math_cache: MathLayoutCache,
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
            wrap_key: WrapKey::default(),
            scroll: 0.0,
            content_h: 0.0,
            math_cache: MathLayoutCache::default(),
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
    #[allow(clippy::too_many_arguments)]
    fn relayout(
        &mut self,
        content_w: f32,
        cell_w: f32,
        cell_h: f32,
        adv_w: f32,
        font_pixel_size: f32,
        pixels_per_point: f32,
        text_ascent: f32,
        scale: f32,
    ) {
        let key = WrapKey {
            content_width: content_w as u32,
            cell_width: (cell_w * 64.0) as u32,
            cell_height: (cell_h * 64.0) as u32,
            text_advance: (adv_w * 64.0) as u32,
            font_pixel_size: (font_pixel_size * 64.0) as u32,
            pixels_per_point: (pixels_per_point * 1024.0) as u32,
            text_ascent: (text_ascent * 64.0) as u32,
            scale: (scale * 1024.0) as u32,
        };
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
            font_pixel_size,
            pixels_per_point,
            text_ascent,
            scale,
            math_cache: &mut self.math_cache,
            next_formula_id: 0,
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
            fragment.text().chars().map(move |ch| WrapChar {
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

/// 数学对象按真实像素宽度参与换行，且永远不从 TeX 源码中间拆开。
fn wrap_inline_with_math(
    inline: &FormattedTextInline,
    max_width: f32,
    text_advance: f32,
    formula_id: &mut u64,
    pixel_size: f32,
    pixels_per_point: f32,
    cache: &mut MathLayoutCache,
) -> Vec<Vec<Span>> {
    if !inline.iter().any(FormattedTextFragment::is_math) {
        let max_cols = (max_width / text_advance).max(4.0) as usize;
        return wrap_inline(inline, max_cols);
    }

    let mut lines = Vec::new();
    let mut line = Vec::new();
    let mut line_width = 0.0;
    for (frag_key, fragment) in inline.iter().enumerate() {
        match &fragment.content {
            FragmentContent::Text(_) => {
                for character in fragment.text().chars() {
                    let cols = character.width().unwrap_or(0).max(1);
                    let width = cols as f32 * text_advance;
                    if line_width + width > max_width && !line.is_empty() {
                        lines.push(std::mem::take(&mut line));
                        line_width = 0.0;
                    }
                    push_text_character(&mut line, fragment, frag_key, character, cols);
                    line_width += width;
                }
            },
            FragmentContent::Math(source) => {
                let current_id = *formula_id;
                *formula_id = formula_id.wrapping_add(1);
                let run = measure_math(
                    source.clone(),
                    current_id,
                    pixel_size,
                    pixels_per_point,
                    false,
                    cache,
                )
                .and_then(|run| {
                    fit_math_run(
                        source.clone(),
                        run,
                        current_id,
                        pixel_size,
                        pixels_per_point,
                        false,
                        max_width,
                        cache,
                    )
                });
                match run {
                    Some(run) => {
                        let width = run.metrics.width;
                        if line_width + width > max_width && !line.is_empty() {
                            lines.push(std::mem::take(&mut line));
                            line_width = 0.0;
                        }
                        line.push(Span::from_math(run, fragment, frag_key));
                        line_width += width;
                    },
                    None => {
                        // A failed formula is still document text. Treating
                        // the entire TeX source as one span made long failures
                        // escape the pane instead of honoring normal wrapping.
                        for character in fragment.text().chars() {
                            if character == '\n' {
                                lines.push(std::mem::take(&mut line));
                                line_width = 0.0;
                                continue;
                            }
                            let cols = character.width().unwrap_or(0).max(1);
                            let width = cols as f32 * text_advance;
                            if line_width + width > max_width && !line.is_empty() {
                                lines.push(std::mem::take(&mut line));
                                line_width = 0.0;
                            }
                            push_text_character(&mut line, fragment, frag_key, character, cols);
                            line_width += width;
                        }
                    },
                }
            },
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

fn push_text_character(
    spans: &mut Vec<Span>,
    fragment: &FormattedTextFragment,
    frag_key: usize,
    character: char,
    cols: usize,
) {
    if spans.last().is_none_or(|span| span.frag_key != frag_key || span.math.is_some()) {
        spans.push(Span::from_fragment(fragment, frag_key));
    }
    let span = spans.last_mut().expect("text span inserted");
    span.text.push(character);
    span.cols += cols;
}

fn measure_math(
    source: MathSource,
    formula_id: u64,
    pixel_size: f32,
    pixels_per_point: f32,
    display: bool,
    cache: &mut MathLayoutCache,
) -> Option<MathRun> {
    let key = FormulaCacheKey::new(formula_id, pixel_size, pixels_per_point, display);
    let layout = cache
        .get_or_insert_with(key, || {
            let formula = parse_formula(source.as_str(), display, DEFAULT_LIMITS)?;
            layout_formula(&formula, pixel_size, pixels_per_point, DEFAULT_LIMITS)
        })
        .ok()?;
    Some(MathRun {
        source,
        formula_id,
        pixel_size,
        pixels_per_point,
        display,
        metrics: layout.metrics,
        advance_width: layout.metrics.width,
    })
}

#[allow(clippy::too_many_arguments)]
fn fit_math_run(
    source: MathSource,
    run: MathRun,
    formula_id: u64,
    pixel_size: f32,
    pixels_per_point: f32,
    display: bool,
    max_width: f32,
    cache: &mut MathLayoutCache,
) -> Option<MathRun> {
    if run.advance_width <= max_width {
        return Some(run);
    }

    // Math layout is linear in pixel size. Leave a small rounding margin so
    // the rightmost antialiasing pixel stays inside the reading column.
    let fitted_size = pixel_size * (max_width / run.advance_width) * 0.98;
    if fitted_size < MIN_FITTED_MATH_PX {
        return None;
    }
    measure_math(source, formula_id, fitted_size, pixels_per_point, display, cache)
        .filter(|fitted| fitted.advance_width <= max_width)
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
            strong_ink: false,
            italic: styles.italic,
            strike: styles.strikethrough,
            code: styles.inline_code,
            link: styles.hyperlink.as_ref().map(|crate::markdown::Hyperlink::Url(url)| url.clone()),
            faint: false,
            frag_key,
            math: None,
        }
    }

    fn from_fragment_text(fragment: &FormattedTextFragment, frag_key: usize) -> Self {
        let mut span = Self::from_fragment(fragment, frag_key);
        span.text.push_str(fragment.text());
        span.cols = fragment.text().chars().map(|c| c.width().unwrap_or(0).max(1)).sum();
        span
    }

    fn from_math(run: MathRun, fragment: &FormattedTextFragment, frag_key: usize) -> Self {
        let mut span = Self::from_fragment(fragment, frag_key);
        span.math = Some(run);
        span
    }

    fn display_math(run: MathRun) -> Self {
        Self {
            text: String::new(),
            cols: 0,
            bold: false,
            strong_ink: false,
            italic: false,
            strike: false,
            code: false,
            link: None,
            faint: false,
            frag_key: usize::MAX,
            math: Some(run),
        }
    }

    fn label(text: impl Into<String>, faint: bool) -> Self {
        let text = text.into();
        let cols = text.chars().map(|c| c.width().unwrap_or(0).max(1)).sum();
        Span {
            text,
            cols,
            bold: false,
            strong_ink: false,
            italic: false,
            strike: false,
            code: false,
            link: None,
            faint,
            frag_key: usize::MAX,
            math: None,
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
    font_pixel_size: f32,
    pixels_per_point: f32,
    text_ascent: f32,
    scale: f32,
    math_cache: &'a mut MathLayoutCache,
    next_formula_id: u64,
}

impl LayoutCtx<'_> {
    fn s(&self, v: f32) -> f32 {
        v * self.scale
    }

    fn push(&mut self, h: f32, indent: f32, text_scale: f32, spans: Vec<Span>, decor: Decor) {
        self.push_with_math(h, indent, text_scale, spans, decor, None, false);
    }

    fn push_with_math(
        &mut self,
        h: f32,
        indent: f32,
        text_scale: f32,
        spans: Vec<Span>,
        decor: Decor,
        math_baseline: Option<f32>,
        center_math: bool,
    ) {
        self.out.push(VisualLine {
            y: self.y,
            h,
            indent,
            scale: text_scale,
            spans,
            decor,
            math_baseline,
            center_math,
        });
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
                let text_advance = self.adv_w * text_scale;
                let wrapped = wrap_inline_with_math(
                    &header.text,
                    (self.content_w - indent).max(text_advance * 4.0),
                    text_advance,
                    &mut self.next_formula_id,
                    self.font_pixel_size * text_scale,
                    self.pixels_per_point,
                    self.math_cache,
                );
                let count = wrapped.len();
                for (i, mut spans) in wrapped.into_iter().enumerate() {
                    for span in &mut spans {
                        // Windows synthesizes bold when only the bundled
                        // Regular CJK face is available. Complex glyphs then
                        // develop visibly uneven strokes, so headings keep the
                        // real Regular outline and use strong theme ink.
                        span.strong_ink = true;
                    }
                    let mut decor = decor;
                    // Typora-style underline on H1/H2, on the last wrap line.
                    decor.underline_row = header.heading_size <= 2 && i + 1 == count;
                    let math_height = spans
                        .iter()
                        .filter_map(|span| span.math.as_ref())
                        .map(|run| run.metrics.height)
                        .fold(0.0f32, f32::max);
                    let math_depth = spans
                        .iter()
                        .filter_map(|span| span.math.as_ref())
                        .map(|run| run.metrics.depth)
                        .fold(0.0f32, f32::max);
                    if math_height > 0.0 || math_depth > 0.0 {
                        let ascent = self.text_ascent * text_scale;
                        let baseline = ch * 0.2 + ascent.max(math_height);
                        let h = (ch * text_scale * 1.45)
                            .max(baseline + (ch * text_scale - ascent).max(math_depth) + ch * 0.2);
                        self.push_with_math(
                            h,
                            indent,
                            text_scale,
                            spans,
                            decor,
                            Some(baseline),
                            false,
                        );
                    } else {
                        self.push(ch * text_scale * 1.45, indent, text_scale, spans, decor);
                    }
                }
                self.y += ch * bottom;
            },
            FormattedTextLine::Line(inline) => {
                let available = (self.content_w - indent).max(self.cell_w * 4.0);
                let wrapped = wrap_inline_with_math(
                    inline,
                    available,
                    self.cell_w,
                    &mut self.next_formula_id,
                    self.font_pixel_size,
                    self.pixels_per_point,
                    self.math_cache,
                );
                for spans in wrapped {
                    let math_height = spans
                        .iter()
                        .filter_map(|span| span.math.as_ref())
                        .map(|run| run.metrics.height)
                        .fold(0.0f32, f32::max);
                    let math_depth = spans
                        .iter()
                        .filter_map(|span| span.math.as_ref())
                        .map(|run| run.metrics.depth)
                        .fold(0.0f32, f32::max);
                    if math_height > 0.0 || math_depth > 0.0 {
                        let top_pad = ch * 0.2;
                        let bottom_pad = ch * 0.2;
                        let baseline = top_pad + self.text_ascent.max(math_height);
                        let h = baseline + (ch - self.text_ascent).max(math_depth) + bottom_pad;
                        self.push_with_math(h, indent, 1.0, spans, decor, Some(baseline), false);
                    } else {
                        self.push(ch * BODY_LINE, indent, 1.0, spans, decor);
                    }
                }
                self.y += ch * 0.5;
            },
            FormattedTextLine::DisplayMath(math) => {
                self.y += ch * 0.35;
                let formula_id = self.next_formula_id;
                self.next_formula_id = self.next_formula_id.wrapping_add(1);
                let max_math_width = (self.content_w - indent).max(self.cell_w * 4.0);
                let run = measure_math(
                    math.clone(),
                    formula_id,
                    self.font_pixel_size,
                    self.pixels_per_point,
                    true,
                    self.math_cache,
                )
                .and_then(|run| {
                    fit_math_run(
                        math.clone(),
                        run,
                        formula_id,
                        self.font_pixel_size,
                        self.pixels_per_point,
                        true,
                        max_math_width,
                        self.math_cache,
                    )
                });
                if let Some(run) = run {
                    let top_pad = ch * 0.45;
                    let bottom_pad = ch * 0.45;
                    let baseline = top_pad + run.metrics.height;
                    let h = baseline + run.metrics.depth + bottom_pad;
                    self.push_with_math(
                        h,
                        indent,
                        1.0,
                        vec![Span::display_math(run)],
                        decor,
                        Some(baseline),
                        true,
                    );
                } else {
                    let fallback = vec![FormattedTextFragment::plain_text(math.as_str())];
                    for mut spans in wrap_inline(&fallback, cols_at(indent, 1.0)) {
                        for span in &mut spans {
                            span.code = true;
                        }
                        self.push(ch * BODY_LINE, indent, 1.0, spans, decor);
                    }
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
        let wrapped = wrap_inline_with_math(
            text,
            cols as f32 * self.cell_w,
            self.cell_w,
            &mut self.next_formula_id,
            self.font_pixel_size,
            self.pixels_per_point,
            self.math_cache,
        );
        for (i, mut spans) in wrapped.into_iter().enumerate() {
            let line_indent = if i == 0 {
                spans.insert(0, Span::label(marker, true));
                indent
            } else {
                // Hanging indent: wrap lines align under the text, not the
                // marker.
                indent + marker_px
            };
            let math_height = spans
                .iter()
                .filter_map(|span| span.math.as_ref())
                .map(|run| run.metrics.height)
                .fold(0.0f32, f32::max);
            let math_depth = spans
                .iter()
                .filter_map(|span| span.math.as_ref())
                .map(|run| run.metrics.depth)
                .fold(0.0f32, f32::max);
            if math_height > 0.0 || math_depth > 0.0 {
                let baseline = ch * 0.15 + self.text_ascent.max(math_height);
                let h = (ch * BODY_LINE * 0.98)
                    .max(baseline + (ch - self.text_ascent).max(math_depth) + ch * 0.15);
                self.push_with_math(h, line_indent, 1.0, spans, decor, Some(baseline), false);
            } else {
                self.push(ch * BODY_LINE * 0.98, line_indent, 1.0, spans, decor);
            }
        }
        self.y += ch * 0.18;
    }

    /// Fixed-grid table: columns sized to their widest cell (bounded by an
    /// even split of the available width), cells truncated with `…` — the
    /// one place the viewer truncates instead of wrapping.
    fn table(&mut self, table: &crate::markdown::FormattedTable, quote_depth: usize, indent: f32) {
        let ch = self.cell_h;
        let col_count =
            table.headers.len().max(table.rows.iter().map(Vec::len).max().unwrap_or(0)).max(1);
        let total_cols = ((self.content_w - indent) / self.cell_w) as usize;
        // 3 columns of separator (" │ ") between cells.
        let usable = total_cols.saturating_sub((col_count - 1) * 3).max(col_count * 4);
        let fair = usable / col_count;
        let mut widths = vec![0usize; col_count];
        for (i, cell) in table.headers.iter().enumerate() {
            widths[i] = widths[i].max(self.inline_columns(cell));
        }
        for row in &table.rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(self.inline_columns(cell));
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
                for mut span in self.table_cell_spans(row.get(i), *width, alignment) {
                    span.bold |= is_head;
                    spans.push(span);
                }
            }
            let mut decor = decor;
            decor.underline_row = is_head;
            let math_height = spans
                .iter()
                .filter_map(|span| span.math.as_ref())
                .map(|run| run.metrics.height)
                .fold(0.0f32, f32::max);
            let math_depth = spans
                .iter()
                .filter_map(|span| span.math.as_ref())
                .map(|run| run.metrics.depth)
                .fold(0.0f32, f32::max);
            if math_height > 0.0 || math_depth > 0.0 {
                let baseline = ch * 0.12 + self.text_ascent.max(math_height);
                let h = (ch * BODY_LINE * 0.95)
                    .max(baseline + (ch - self.text_ascent).max(math_depth) + ch * 0.12);
                self.push_with_math(h, indent, 1.0, spans, decor, Some(baseline), false);
            } else {
                self.push(ch * BODY_LINE * 0.95, indent, 1.0, spans, decor);
            }
        }
        self.y += ch * 0.6;
    }

    fn inline_columns(&mut self, inline: &FormattedTextInline) -> usize {
        inline
            .iter()
            .map(|fragment| match &fragment.content {
                FragmentContent::Text(_) => fragment
                    .text()
                    .chars()
                    .map(|character| character.width().unwrap_or(0).max(1))
                    .sum(),
                FragmentContent::Math(source) => {
                    let formula_id = self.next_formula_id;
                    self.next_formula_id = self.next_formula_id.wrapping_add(1);
                    measure_math(
                        source.clone(),
                        formula_id,
                        self.font_pixel_size,
                        self.pixels_per_point,
                        false,
                        self.math_cache,
                    )
                    .map_or_else(
                        || fragment.text().chars().count(),
                        |run| (run.metrics.width / self.cell_w).ceil().max(1.0) as usize,
                    )
                },
            })
            .sum()
    }

    fn table_cell_spans(
        &mut self,
        cell: Option<&FormattedTextInline>,
        width: usize,
        alignment: TableAlignment,
    ) -> Vec<Span> {
        if cell.is_none_or(|inline| !inline.iter().any(FormattedTextFragment::is_math)) {
            return cell_spans(cell, width, alignment);
        }

        let Some(inline) = cell else { return padded_spans(Vec::new(), 0, width, alignment) };
        let mut spans = Vec::new();
        let mut used = 0usize;
        'outer: for (frag_key, fragment) in inline.iter().enumerate() {
            match &fragment.content {
                FragmentContent::Text(_) => {
                    for character in fragment.text().chars() {
                        let columns = character.width().unwrap_or(0).max(1);
                        if used + columns > width {
                            push_ellipsis(&mut spans, &mut used, width);
                            break 'outer;
                        }
                        push_text_character(&mut spans, fragment, frag_key, character, columns);
                        used += columns;
                    }
                },
                FragmentContent::Math(source) => {
                    let formula_id = self.next_formula_id;
                    self.next_formula_id = self.next_formula_id.wrapping_add(1);
                    let run = measure_math(
                        source.clone(),
                        formula_id,
                        self.font_pixel_size,
                        self.pixels_per_point,
                        false,
                        self.math_cache,
                    );
                    let columns = run.as_ref().map_or_else(
                        || fragment.text().chars().count(),
                        |run| (run.metrics.width / self.cell_w).ceil().max(1.0) as usize,
                    );
                    if used + columns > width {
                        push_ellipsis(&mut spans, &mut used, width);
                        break 'outer;
                    }
                    match run {
                        Some(mut run) => {
                            run.advance_width = columns as f32 * self.cell_w;
                            spans.push(Span::from_math(run, fragment, frag_key));
                        },
                        None => spans.push(Span::from_fragment_text(fragment, frag_key)),
                    }
                    used += columns;
                },
            }
        }
        padded_spans(spans, used, width, alignment)
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
            for ch in fragment.text().chars() {
                let cols = ch.width().unwrap_or(0).max(1);
                if used + cols > width {
                    // No room left: truncate with an ellipsis if one fits.
                    push_ellipsis(&mut spans, &mut used, width);
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
    padded_spans(spans, used, width, alignment)
}

fn push_ellipsis(spans: &mut Vec<Span>, used: &mut usize, width: usize) {
    if *used < width {
        spans.push(Span::label("…", true));
        *used += 1;
    }
}

fn padded_spans(
    mut spans: Vec<Span>,
    used: usize,
    width: usize,
    alignment: TableAlignment,
) -> Vec<Span> {
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

fn span_width(span: &Span, text_advance: f32) -> f32 {
    span.math.as_ref().map_or(span.cols as f32 * text_advance, |run| run.advance_width)
}

fn line_width(line: &VisualLine, cell_w: f32, adv_base: f32) -> f32 {
    let text_advance = if line.scale == 1.0 { cell_w } else { adv_base * line.scale };
    line.spans.iter().map(|span| span_width(span, text_advance)).sum()
}

fn line_start_x(line: &VisualLine, cx: f32, cw: f32, cell_w: f32, adv_base: f32) -> f32 {
    let left = cx + line.indent;
    if line.center_math {
        left + ((cw - line.indent - line_width(line, cell_w, adv_base)) * 0.5).max(0.0)
    } else {
        left
    }
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
    let font_pixel_size = glyph_cache.font_size.as_px();
    let text_ascent = (cell_h + glyph_cache.font_metrics().descent).max(1.0);
    let pixels_per_point = scale * 96.0 / 72.27;
    let (cx, cy, cw, chh) = column_rect(area, scale);
    // Glyphs must land on whole physical pixels: the layout accumulates
    // fractional heights (1.55 line heights, centered columns), and drawing
    // at sub-pixel offsets bilinear-samples every glyph into a blur.
    let (cx, cy) = (cx.round(), cy.round());

    doc.relayout(
        cw,
        cell_w,
        cell_h,
        adv_base,
        font_pixel_size,
        pixels_per_point,
        text_ascent,
        scale,
    );
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
        let mut pen_x = line_start_x(line, cx, cw, cell_w, adv_base);
        for span in &line.spans {
            let w = span_width(span, span_adv);
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
        let text_ascent = (cell_h + glyph_cache.font_metrics().descent).max(1.0) * line.scale;
        let text_y = line.math_baseline.map_or_else(
            || (y + (line.h - cell_h * line.scale) / 2.0).round(),
            |baseline| (y + baseline - text_ascent).round(),
        );
        let text_visible = text_y >= clip_top && text_y + cell_h * line.scale <= clip_bot;
        let mut pen_x = line_start_x(line, cx, cw, cell_w, adv_base);
        for span in &line.spans {
            let ink = if span.link.is_some() {
                skin.accent
            } else if span.code {
                skin.ink_strong
            } else if span.faint {
                skin.ink_faint
            } else if span.bold || span.strong_ink {
                skin.ink_strong
            } else if span.italic {
                skin.ink_dim
            } else {
                skin.ink
            };
            let span_adv = if line.scale == 1.0 { cell_w } else { adv_base * line.scale };
            if let Some(run) = &span.math {
                let key = FormulaCacheKey::new(
                    run.formula_id,
                    run.pixel_size,
                    run.pixels_per_point,
                    run.display,
                );
                let layout = doc.math_cache.get_or_insert_with(key, || {
                    let formula = parse_formula(run.source.as_str(), run.display, DEFAULT_LIMITS)?;
                    layout_formula(&formula, run.pixel_size, run.pixels_per_point, DEFAULT_LIMITS)
                });
                let baseline_y = y + line.math_baseline.unwrap_or(text_ascent);
                match layout {
                    Ok(layout) => {
                        let math_drawn = renderer
                            .draw_math(
                                size,
                                layout,
                                pen_x.round(),
                                baseline_y.round(),
                                ink,
                                MathClip {
                                    left: cx,
                                    top: clip_top,
                                    right: cx + cw,
                                    bottom: clip_bot,
                                },
                            )
                            .is_ok();
                        if math_drawn {
                            // Characters outside Latin Modern Math (notably
                            // Chinese prose in existing note files) reuse the
                            // document font cache instead of allocating a
                            // second formula atlas or raster image.
                            let base_text_ascent = text_ascent / line.scale.max(f32::EPSILON);
                            for op in &layout.text {
                                let op_scale = line.scale * op.pixel_size / run.pixel_size;
                                let op_baseline = baseline_y + op.baseline_y;
                                let op_y = (op_baseline - base_text_ascent * op_scale).round();
                                let op_x = (pen_x + op.x).round();
                                let op_columns = op.character.width().unwrap_or(1).max(1) as f32;
                                let op_width = cell_w * op_columns * op_scale;
                                let op_height = cell_h * op_scale;
                                if op_x < cx
                                    || op_x + op_width > cx + cw
                                    || op_y < clip_top
                                    || op_y + op_height > clip_bot
                                {
                                    continue;
                                }
                                let mut buffer = [0u8; 4];
                                let text = op.character.encode_utf8(&mut buffer);
                                renderer.draw_doc_text(
                                    size,
                                    op_x,
                                    op_y,
                                    op_scale,
                                    ink,
                                    Flags::empty(),
                                    text,
                                    glyph_cache,
                                );
                            }
                        } else if text_visible {
                            renderer.draw_doc_text(
                                size,
                                pen_x.round(),
                                text_y,
                                line.scale,
                                ink,
                                Flags::empty(),
                                run.source.as_str(),
                                glyph_cache,
                            );
                        }
                    },
                    Err(_) if text_visible => {
                        renderer.draw_doc_text(
                            size,
                            pen_x.round(),
                            text_y,
                            line.scale,
                            ink,
                            Flags::empty(),
                            run.source.as_str(),
                            glyph_cache,
                        );
                    },
                    Err(_) => {},
                }
                pen_x += run.advance_width;
                continue;
            }

            if !text_visible {
                pen_x += span.cols as f32 * span_adv;
                continue;
            }

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
    fn inline_math_is_one_wrap_object_with_native_metrics() {
        let parsed = crate::markdown::parse_markdown("before $\\frac{x}{y}$ after");
        let FormattedTextLine::Line(inline) = &parsed.lines[0] else { panic!() };
        let mut cache = MathLayoutCache::default();
        let mut formula_id = 0;
        let lines =
            wrap_inline_with_math(inline, 90.0, 8.0, &mut formula_id, 16.0, 1.0, &mut cache);
        let math: Vec<&MathRun> = lines
            .iter()
            .flat_map(|line| line.iter())
            .filter_map(|span| span.math.as_ref())
            .collect();
        assert_eq!(math.len(), 1);
        assert_eq!(math[0].source.as_str(), r"\frac{x}{y}");
        assert!(math[0].metrics.width > 0.0);
        assert!(math[0].metrics.height > 0.0 && math[0].metrics.depth > 0.0);
    }

    #[test]
    fn oversized_inline_math_is_fitted_before_it_can_cross_the_column() {
        let parsed = crate::markdown::parse_markdown(r"before $\frac{a+b+c+d+e}{x+y+z+w+q}$ after");
        let FormattedTextLine::Line(inline) = &parsed.lines[0] else { panic!() };
        let mut cache = MathLayoutCache::default();
        let mut formula_id = 0;
        let max_width = 96.0;
        let lines =
            wrap_inline_with_math(inline, max_width, 8.0, &mut formula_id, 16.0, 1.0, &mut cache);

        assert!(lines.iter().flatten().any(|span| span.math.is_some()));
        assert!(lines.iter().all(|line| { line_width_for_test(line, 8.0) <= max_width }));
    }

    #[test]
    fn display_math_uses_real_height_and_centers_in_the_column() {
        let markdown = "$$\\frac{x+1}{\\sqrt{y}}$$";
        let mut doc = DocView {
            path: PathBuf::from("math.md"),
            title: "math.md".into(),
            blocks: crate::markdown::parse_markdown(markdown).lines.into_iter().collect(),
            visual: Vec::new(),
            wrap_key: WrapKey::default(),
            scroll: 0.0,
            content_h: 0.0,
            math_cache: MathLayoutCache::default(),
        };
        doc.relayout(600.0, 8.0, 16.0, 8.0, 16.0, 1.0, 12.0, 1.0);
        assert_eq!(doc.visual.len(), 1);
        let line = &doc.visual[0];
        assert!(line.center_math);
        assert!(line.math_baseline.is_some());
        assert!(line.h > 16.0);
        assert!(line.spans[0].math.is_some());
    }

    #[test]
    fn headings_use_strong_ink_without_forcing_synthetic_cjk_bold() {
        let mut doc = DocView {
            path: PathBuf::from("heading.md"),
            title: "heading.md".into(),
            blocks: crate::markdown::parse_markdown("# 微积分").lines.into_iter().collect(),
            visual: Vec::new(),
            wrap_key: WrapKey::default(),
            scroll: 0.0,
            content_h: 0.0,
            math_cache: MathLayoutCache::default(),
        };
        doc.relayout(600.0, 8.0, 16.0, 8.0, 16.0, 1.0, 12.0, 1.0);
        let span = &doc.visual[0].spans[0];

        assert!(span.strong_ink);
        assert!(!span.bold);
    }

    #[test]
    fn invalid_math_keeps_source_text_visible() {
        let parsed = crate::markdown::parse_markdown("before $\\def\\x{1}$ after");
        let FormattedTextLine::Line(inline) = &parsed.lines[0] else { panic!() };
        let mut cache = MathLayoutCache::default();
        let mut formula_id = 0;
        let lines =
            wrap_inline_with_math(inline, 500.0, 8.0, &mut formula_id, 16.0, 1.0, &mut cache);
        assert!(lines.iter().flatten().all(|span| span.math.is_none()));
        assert!(spans_text(&lines[0]).contains(r"\def\x{1}"));
    }

    #[test]
    fn invalid_inline_and_display_math_fallbacks_wrap_to_the_content_width() {
        let parsed = crate::markdown::parse_markdown("before $\\def\\x{123456789}$ after");
        let FormattedTextLine::Line(inline) = &parsed.lines[0] else { panic!() };
        let mut cache = MathLayoutCache::default();
        let mut formula_id = 0;
        let lines =
            wrap_inline_with_math(inline, 48.0, 8.0, &mut formula_id, 16.0, 1.0, &mut cache);
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|line| line_width_for_test(line, 8.0) <= 48.0));

        let mut doc = DocView {
            path: PathBuf::from("invalid-display.md"),
            title: "invalid-display.md".into(),
            blocks: crate::markdown::parse_markdown("$$\\def\\x{12345678901234567890}$$")
                .lines
                .into_iter()
                .collect(),
            visual: Vec::new(),
            wrap_key: WrapKey::default(),
            scroll: 0.0,
            content_h: 0.0,
            math_cache: MathLayoutCache::default(),
        };
        doc.relayout(80.0, 8.0, 16.0, 8.0, 16.0, 1.0, 12.0, 1.0);
        assert!(doc.visual.len() > 1);
        assert!(doc.visual.iter().all(|line| !line.center_math && line.spans[0].code));
    }

    #[test]
    fn quoted_bare_tex_stays_text_and_wraps_inside_the_reading_column() {
        let markdown = concat!(
            "> \\lim_{x \\to x_0} f(x)=A\\iff",
            "f(x)=A+\\alpha(x)\\frac{123456789}{987654321}\\sqrt{x^2+y^2}",
        );
        let mut doc = DocView {
            path: PathBuf::from("bare-tex.md"),
            title: "bare-tex.md".into(),
            blocks: crate::markdown::parse_markdown(markdown).lines.into_iter().collect(),
            visual: Vec::new(),
            wrap_key: WrapKey::default(),
            scroll: 0.0,
            content_h: 0.0,
            math_cache: MathLayoutCache::default(),
        };
        let content_width = 120.0;
        doc.relayout(content_width, 8.0, 16.0, 8.0, 16.0, 1.0, 12.0, 1.0);

        assert!(doc.visual.len() > 1);
        assert!(doc.visual.iter().all(|line| {
            line.spans.iter().all(|span| span.math.is_none())
                && line.indent + line_width_for_test(&line.spans, 8.0) <= content_width
        }));
    }

    fn line_width_for_test(line: &[Span], text_advance: f32) -> f32 {
        line.iter().map(|span| span_width(span, text_advance)).sum()
    }

    #[test]
    fn quoted_multiline_chinese_formula_from_real_notes_uses_native_layout() {
        let markdown = concat!(
            "> $$\n",
            "> (\\ln(\\sqrt{1+x^2} + x))是奇函数 \\\\\n",
            "> e^x+e^{-x}是偶函数\\\\\n",
            "> f(x)+f(-x)是偶函数，f(x)-f(-x)是奇函数\n",
            "> $$",
        );
        let mut doc = DocView {
            path: PathBuf::from("real-note.md"),
            title: "real-note.md".into(),
            blocks: crate::markdown::parse_markdown(markdown).lines.into_iter().collect(),
            visual: Vec::new(),
            wrap_key: WrapKey::default(),
            scroll: 0.0,
            content_h: 0.0,
            math_cache: MathLayoutCache::default(),
        };
        doc.relayout(600.0, 8.0, 16.0, 8.0, 16.0, 1.0, 12.0, 1.0);
        let run = doc
            .visual
            .iter()
            .flat_map(|line| &line.spans)
            .find_map(|span| span.math.as_ref())
            .expect("real note formula should remain native math");
        let key =
            FormulaCacheKey::new(run.formula_id, run.pixel_size, run.pixels_per_point, run.display);
        let layout = doc.math_cache.get(key).expect("layout cached during measurement");
        assert!(!layout.glyphs.is_empty());
        assert!(!layout.text.is_empty());
    }

    #[test]
    fn auxiliary_angle_formula_with_blank_rows_and_prose_uses_native_layout() {
        let markdown = concat!(
            "#### 辅助角公式\n\n",
            "$$\n",
            r"\sin x + \cos x = \sqrt{2} \sin(x + \frac{\pi}{4}) \\",
            "\n\n",
            r"\sin x + \cos x = \sqrt{2} \cos(x - \frac{\pi}{4}) \\",
            "\n\n",
            r"a \sin x + b \cos x = \sqrt{a^2 + b^2} \cos(x - \varphi) \quad ",
            r"\left(b > 0, \varphi \in \left(-\frac{\pi}{2}, \frac{\pi}{2}\right), ",
            r"\tan \varphi = \frac{a}{b}\right)",
            "\n\n",
            r"这里使用了和角公式：\\",
            "\n",
            r"\sin(a + b) = \sin a \cos b + \cos a \sin b\\",
            "\n",
            r"将 a = x 和 b = \frac{\pi}{4} 代入，因为 ",
            r"\sin\frac{\pi}{4} = \cos\frac{\pi}{4} = \frac{\sqrt{2}}{2}，所以：\\",
            "\n",
            r"\sin(a+\frac{\pi}{4})= \sin x \cdot \frac{\sqrt{2}}{2} ",
            r"+ \cos x \cdot \frac{\sqrt{2}}{2}\\",
            "\n",
            r"= \frac{\sqrt{2}}{2}(\sin x + \cos x)\\",
            "\n",
            r"(\sin x + \cos x) = \sqrt{2}\sin\left(x + \frac{\pi}{4}\right) \\",
            "\n",
            "同时乘以根号 2 即可得出\n",
            "$$",
        );
        let mut doc = DocView {
            path: PathBuf::from("auxiliary-angle.md"),
            title: "auxiliary-angle.md".into(),
            blocks: crate::markdown::parse_markdown(markdown).lines.into_iter().collect(),
            visual: Vec::new(),
            wrap_key: WrapKey::default(),
            scroll: 0.0,
            content_h: 0.0,
            math_cache: MathLayoutCache::default(),
        };
        doc.relayout(860.0, 8.0, 16.0, 8.0, 16.0, 1.0, 12.0, 1.0);

        assert!(
            doc.visual.iter().any(|line| {
                line.center_math && line.spans.iter().any(|span| span.math.is_some())
            })
        );
        assert!(
            doc.visual
                .iter()
                .all(|line| { !line.spans.iter().any(|span| span.text.starts_with("$$")) })
        );
        assert!(doc.visual.iter().all(|line| {
            line.spans
                .iter()
                .all(|span| span.math.as_ref().is_none_or(|run| run.advance_width <= 860.0))
        }));
    }

    #[test]
    fn math_rendering_fixture_uses_native_layout_for_every_formula() {
        let document =
            crate::markdown::parse_markdown(include_str!("../../../docs/math-rendering-test.md"));
        let mut formula_count = 0usize;
        let mut check = |source: &MathSource, display: bool| {
            let formula = parse_formula(source.as_str(), display, DEFAULT_LIMITS)
                .unwrap_or_else(|error| panic!("fixture parse failed for {:?}: {error:?}", source));
            let layout =
                layout_formula(&formula, 18.0, 1.0, DEFAULT_LIMITS).unwrap_or_else(|error| {
                    panic!("fixture layout failed for {:?}: {error:?}", source)
                });
            assert!(layout.metrics.width.is_finite() && layout.metrics.width > 0.0);
            formula_count += 1;
        };

        for line in &document.lines {
            match line {
                FormattedTextLine::DisplayMath(source) => check(source, true),
                FormattedTextLine::Line(inline)
                | FormattedTextLine::Heading(crate::markdown::FormattedTextHeader {
                    text: inline,
                    ..
                }) => {
                    for fragment in inline {
                        if let FragmentContent::Math(source) = &fragment.content {
                            check(source, false);
                        }
                    }
                },
                _ => {},
            }
        }

        assert!(formula_count >= 25, "fixture should cover many formulas");
    }

    #[test]
    fn table_cells_keep_math_as_native_objects() {
        let markdown = "| expression | value |\n| --- | --- |\n| $x^2$ | $\\frac{1}{2}$ |";
        let mut doc = DocView {
            path: PathBuf::from("math-table.md"),
            title: "math-table.md".into(),
            blocks: crate::markdown::parse_markdown(markdown).lines.into_iter().collect(),
            visual: Vec::new(),
            wrap_key: WrapKey::default(),
            scroll: 0.0,
            content_h: 0.0,
            math_cache: MathLayoutCache::default(),
        };
        doc.relayout(600.0, 8.0, 16.0, 8.0, 16.0, 1.0, 12.0, 1.0);
        let math_runs = doc
            .visual
            .iter()
            .flat_map(|line| &line.spans)
            .filter(|span| span.math.is_some())
            .count();
        assert_eq!(math_runs, 2);
        assert!(doc.visual.iter().any(|line| line.math_baseline.is_some()));
    }

    #[test]
    fn visual_layout_is_virtualizable() {
        let markdown = (0..500).map(|i| format!("paragraph {i}\n\n")).collect::<String>();
        let mut doc = DocView {
            path: PathBuf::from("test.md"),
            title: "test.md".into(),
            blocks: crate::markdown::parse_markdown(&markdown).lines.into_iter().collect(),
            visual: Vec::new(),
            wrap_key: WrapKey::default(),
            scroll: 0.0,
            content_h: 0.0,
            math_cache: MathLayoutCache::default(),
        };
        doc.relayout(600.0, 8.0, 16.0, 8.0, 16.0, 1.0, 12.0, 1.0);
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
