//! pulldown-latex 事件流到有界 Math arena。

use std::borrow::Cow;
use std::mem;

use pulldown_latex::event::{
    ArrayColumn, ColumnAlignment, Content, DelimiterSize, DelimiterType, EnvironmentFlow, Event,
    Font, Grouping, ScriptPosition, ScriptType, StateChange, Style, Visual,
};
use pulldown_latex::{Parser, Storage};

use super::ir::{
    AlignRange, AtomClass, ChildRange, ColumnAlign, DelimiterScale, FontVariant, MathArena,
    MathNode, MathStyle, MatrixRow, NodeId, ParsedFormula, RowRange, ScriptPlacement,
    StyleOverride,
};
use super::{MathError, MathErrorKind, MathLimits, validate};

pub(crate) fn parse_formula(
    source: &str,
    display: bool,
    limits: MathLimits,
) -> Result<ParsedFormula, MathError> {
    validate(source, limits)?;

    let style = if display { MathStyle::Display } else { MathStyle::Text };
    // pulldown-latex intentionally rejects `\\` outside an alignment
    // environment. Markdown math blocks commonly use it directly, so wrap
    // only formulas that contain an environment-external line break in a
    // one-column gathered environment. Existing matrix/align rows retain
    // their own scope and are not double-wrapped.
    let parser_source = if display && has_unscoped_line_break(source) {
        Cow::Owned(format!(r"\begin{{gathered}}{source}\end{{gathered}}"))
    } else {
        Cow::Borrowed(source)
    };
    let storage = Storage::new();
    let mut builder = Builder::new(style, limits);
    let mut event_count = 0usize;
    for event in Parser::new(parser_source.as_ref(), &storage) {
        if event_count >= limits.max_events {
            return Err(MathError::new(MathErrorKind::EventLimit, source.len()));
        }
        event_count += 1;
        let event = event.map_err(|_| MathError::new(MathErrorKind::Parse, 0))?;
        builder.consume(event)?;
    }
    let (arena, root) = builder.finish()?;
    Ok(ParsedFormula { arena, root, style, event_count })
}

/// Detect `\\` outside `\begin{...}` environments in one linear scan.
/// Comments are skipped because a line break command there is inert.
fn has_unscoped_line_break(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut environment_depth = 0usize;
    let mut offset = 0usize;
    while offset < bytes.len() {
        match bytes[offset] {
            b'%' => {
                offset += 1;
                while offset < bytes.len() && bytes[offset] != b'\n' {
                    offset += 1;
                }
            },
            b'\\' if bytes.get(offset + 1) == Some(&b'\\') => {
                if environment_depth == 0 {
                    return true;
                }
                offset += 2;
            },
            b'\\' => {
                offset += 1;
                let command_start = offset;
                while offset < bytes.len()
                    && (bytes[offset].is_ascii_alphabetic() || bytes[offset] == b'@')
                {
                    offset += 1;
                }
                let command = &source[command_start..offset];
                if matches!(command, "begin" | "end") {
                    while offset < bytes.len() && bytes[offset].is_ascii_whitespace() {
                        offset += 1;
                    }
                    if bytes.get(offset) == Some(&b'{') {
                        if let Some(end) = bytes[offset + 1..].iter().position(|byte| *byte == b'}')
                        {
                            offset += end + 2;
                            if command == "begin" {
                                environment_depth = environment_depth.saturating_add(1);
                            } else {
                                environment_depth = environment_depth.saturating_sub(1);
                            }
                        }
                    }
                } else if command.is_empty() {
                    offset += source[offset..].chars().next().map(char::len_utf8).unwrap_or(0);
                }
            },
            byte if byte.is_ascii() => offset += 1,
            _ => offset += source[offset..].chars().next().map(char::len_utf8).unwrap_or(1),
        }
    }
    false
}

#[derive(Clone, Copy, Debug)]
struct ParseState {
    variant: FontVariant,
    style_override: Option<StyleOverride>,
}

enum Context {
    Container(Container),
    Pending(Pending),
}

struct Container {
    kind: ContainerKind,
    state: ParseState,
    structural_style_override: Option<StyleOverride>,
    elements: Vec<NodeId>,
}

enum ContainerKind {
    Root,
    Group,
    LeftRight { open: Option<char>, close: Option<char> },
    Matrix(MatrixBuild),
}

struct MatrixBuild {
    alignments: Vec<ColumnAlign>,
    cells: Vec<NodeId>,
    row_lengths: Vec<u16>,
    current_row_start: usize,
    fence: Option<(Option<char>, Option<char>)>,
}

struct Pending {
    kind: PendingKind,
    children: Vec<NodeId>,
    expected: usize,
}

enum PendingKind {
    Fraction { bar: bool, style_override: Option<StyleOverride> },
    SquareRoot { style_override: Option<StyleOverride> },
    Root { style_override: Option<StyleOverride> },
    Scripts { ty: ScriptType, placement: ScriptPlacement, style_override: Option<StyleOverride> },
    Negation { style_override: Option<StyleOverride> },
}

struct Builder {
    arena: MathArena,
    contexts: Vec<Context>,
    limits: MathLimits,
    matrix_cell_count: usize,
    next_style_scope: u16,
}

impl Builder {
    fn new(_style: MathStyle, limits: MathLimits) -> Self {
        let root = Container {
            kind: ContainerKind::Root,
            state: ParseState { variant: FontVariant::Normal, style_override: None },
            structural_style_override: None,
            elements: Vec::new(),
        };
        Self {
            arena: MathArena::default(),
            contexts: vec![Context::Container(root)],
            limits,
            matrix_cell_count: 0,
            next_style_scope: 0,
        }
    }

    fn consume(&mut self, event: Event<'_>) -> Result<(), MathError> {
        match event {
            Event::Content(content) => {
                let state = self.current_state()?;
                let node = self.content_node(content, state)?;
                self.deliver(node)
            },
            Event::Begin(grouping) => self.begin(grouping),
            Event::End => self.end(),
            Event::Visual(visual) => self.pending_visual(visual),
            Event::Script { ty, position } => {
                let style_override = self.current_state()?.style_override;
                let expected = match ty {
                    ScriptType::Subscript | ScriptType::Superscript => 2,
                    ScriptType::SubSuperscript => 3,
                };
                self.push_context(Context::Pending(Pending {
                    kind: PendingKind::Scripts {
                        ty,
                        placement: script_placement(position),
                        style_override,
                    },
                    children: Vec::with_capacity(expected),
                    expected,
                }))
            },
            Event::Space { width, height } => {
                let style_override = self.current_state()?.style_override;
                let node = self.add_node(MathNode::Space { width, height, style_override })?;
                self.deliver(node)
            },
            Event::StateChange(change) => self.state_change(change),
            Event::EnvironmentFlow(flow) => self.environment_flow(flow),
        }
    }

    fn current_state(&self) -> Result<ParseState, MathError> {
        self.contexts
            .iter()
            .rev()
            .find_map(|context| match context {
                Context::Container(container) => Some(container.state),
                Context::Pending(_) => None,
            })
            .ok_or_else(|| MathError::new(MathErrorKind::Parse, 0))
    }

    fn push_context(&mut self, context: Context) -> Result<(), MathError> {
        // Root 不计入 TeX 深度；Pending 也必须受同一上限，防止无花括号前缀链堆积。
        if self.contexts.len() > self.limits.max_depth {
            return Err(MathError::new(MathErrorKind::NestingTooDeep, 0));
        }
        self.contexts.push(context);
        Ok(())
    }

    fn begin(&mut self, grouping: Grouping) -> Result<(), MathError> {
        let state = self.current_state()?;
        let kind = match grouping {
            Grouping::Normal => ContainerKind::Group,
            Grouping::LeftRight(open, close) => ContainerKind::LeftRight { open, close },
            grouping => {
                let (alignments, fence) = matrix_shape(&grouping);
                ContainerKind::Matrix(MatrixBuild {
                    alignments,
                    cells: Vec::new(),
                    row_lengths: Vec::new(),
                    current_row_start: 0,
                    fence,
                })
            },
        };
        self.push_context(Context::Container(Container {
            kind,
            state,
            structural_style_override: state.style_override,
            elements: Vec::new(),
        }))
    }

    fn end(&mut self) -> Result<(), MathError> {
        if self.contexts.len() <= 1 {
            return Err(MathError::new(MathErrorKind::Parse, 0));
        }
        let Some(Context::Container(container)) = self.contexts.pop() else {
            return Err(MathError::new(MathErrorKind::Parse, 0));
        };
        let node = self.finish_container(container)?;
        self.deliver(node)
    }

    fn pending_visual(&mut self, visual: Visual) -> Result<(), MathError> {
        let style_override = self.current_state()?.style_override;
        let (kind, expected) = match visual {
            Visual::Fraction(thickness) => (
                PendingKind::Fraction {
                    bar: !matches!(thickness, Some(value) if value.value == 0.0),
                    style_override,
                },
                2,
            ),
            Visual::SquareRoot => (PendingKind::SquareRoot { style_override }, 1),
            Visual::Root => (PendingKind::Root { style_override }, 2),
            Visual::Negation => (PendingKind::Negation { style_override }, 1),
        };
        self.push_context(Context::Pending(Pending {
            kind,
            children: Vec::with_capacity(expected),
            expected,
        }))
    }

    fn state_change(&mut self, change: StateChange) -> Result<(), MathError> {
        let Some(container) = self.contexts.iter_mut().rev().find_map(|context| match context {
            Context::Container(container) => Some(container),
            Context::Pending(_) => None,
        }) else {
            return Err(MathError::new(MathErrorKind::Parse, 0));
        };

        match change {
            StateChange::Font(font) => {
                container.state.variant = font.map(font_variant).unwrap_or(FontVariant::Normal)
            },
            StateChange::Style(style) => {
                let scope = self.next_style_scope;
                self.next_style_scope = self
                    .next_style_scope
                    .checked_add(1)
                    .ok_or_else(|| MathError::new(MathErrorKind::EventLimit, 0))?;
                container.state.style_override =
                    Some(StyleOverride { style: math_style(style), scope });
            },
            StateChange::Color(_) => {},
        }
        Ok(())
    }

    fn environment_flow(&mut self, flow: EnvironmentFlow) -> Result<(), MathError> {
        let Some(Context::Container(container)) = self.contexts.last() else {
            return Err(MathError::new(MathErrorKind::Parse, 0));
        };
        if !matches!(container.kind, ContainerKind::Matrix(_)) {
            return Err(MathError::new(MathErrorKind::Parse, 0));
        }

        match flow {
            EnvironmentFlow::Alignment => self.finish_matrix_cell(),
            EnvironmentFlow::NewLine { .. } => {
                self.finish_matrix_cell()?;
                self.finish_matrix_row()
            },
            EnvironmentFlow::StartLines { .. } => Ok(()),
        }
    }

    fn finish_matrix_cell(&mut self) -> Result<(), MathError> {
        if self.matrix_cell_count >= self.limits.max_matrix_cells {
            return Err(MathError::new(MathErrorKind::MatrixCellLimit, 0));
        }
        let elements = match self.contexts.last_mut() {
            Some(Context::Container(container))
                if matches!(container.kind, ContainerKind::Matrix(_)) =>
            {
                mem::take(&mut container.elements)
            },
            _ => return Err(MathError::new(MathErrorKind::Parse, 0)),
        };
        let cell = self.add_row(elements)?;
        let Some(Context::Container(Container { kind: ContainerKind::Matrix(matrix), .. })) =
            self.contexts.last_mut()
        else {
            return Err(MathError::new(MathErrorKind::Parse, 0));
        };
        matrix.cells.push(cell);
        self.matrix_cell_count += 1;
        Ok(())
    }

    fn finish_matrix_row(&mut self) -> Result<(), MathError> {
        let Some(Context::Container(Container { kind: ContainerKind::Matrix(matrix), .. })) =
            self.contexts.last_mut()
        else {
            return Err(MathError::new(MathErrorKind::Parse, 0));
        };
        let len = matrix.cells.len().saturating_sub(matrix.current_row_start);
        if len > self.limits.max_children || len > u16::MAX as usize {
            return Err(MathError::new(MathErrorKind::ChildLimit, 0));
        }
        matrix.row_lengths.push(len as u16);
        matrix.current_row_start = matrix.cells.len();
        Ok(())
    }

    fn deliver(&mut self, mut node: NodeId) -> Result<(), MathError> {
        loop {
            match self.contexts.last_mut() {
                Some(Context::Container(container)) => {
                    if container.elements.len() >= self.limits.max_children {
                        return Err(MathError::new(MathErrorKind::ChildLimit, 0));
                    }
                    container.elements.push(node);
                    return Ok(());
                },
                Some(Context::Pending(pending)) => {
                    pending.children.push(node);
                    if pending.children.len() < pending.expected {
                        return Ok(());
                    }
                },
                None => return Err(MathError::new(MathErrorKind::Parse, 0)),
            }

            let Some(Context::Pending(pending)) = self.contexts.pop() else {
                return Err(MathError::new(MathErrorKind::Parse, 0));
            };
            node = self.finish_pending(pending)?;
        }
    }

    fn finish_pending(&mut self, pending: Pending) -> Result<NodeId, MathError> {
        let children = pending.children;
        let node = match pending.kind {
            PendingKind::Fraction { bar, style_override } => MathNode::Fraction {
                numerator: children[0],
                denominator: children[1],
                bar,
                style_override,
            },
            PendingKind::SquareRoot { style_override } => {
                MathNode::Radical { degree: None, body: children[0], style_override }
            },
            PendingKind::Root { style_override } => {
                MathNode::Radical { degree: Some(children[1]), body: children[0], style_override }
            },
            PendingKind::Negation { style_override } => {
                MathNode::Negation { body: children[0], style_override }
            },
            PendingKind::Scripts { ty, placement, style_override } => {
                if matches!(ty, ScriptType::Superscript)
                    && matches!(placement, ScriptPlacement::AboveBelow)
                    && let Some(accent) = self.single_character(children[1])
                {
                    MathNode::Accent { accent, body: children[0], style_override }
                } else {
                    let (subscript, superscript) = match ty {
                        ScriptType::Subscript => (Some(children[1]), None),
                        ScriptType::Superscript => (None, Some(children[1])),
                        ScriptType::SubSuperscript => (Some(children[1]), Some(children[2])),
                    };
                    MathNode::Scripts {
                        base: children[0],
                        subscript,
                        superscript,
                        placement,
                        style_override,
                    }
                }
            },
        };
        self.add_node(node)
    }

    fn single_character(&self, node: NodeId) -> Option<char> {
        match self.arena.node(node) {
            MathNode::Glyph { character, .. } | MathNode::Operator { character, .. } => {
                Some(*character)
            },
            MathNode::Row(range) => {
                let [child] = self.arena.children(*range) else { return None };
                self.single_character(*child)
            },
            _ => None,
        }
    }

    fn finish_container(&mut self, mut container: Container) -> Result<NodeId, MathError> {
        match container.kind {
            ContainerKind::Root => Err(MathError::new(MathErrorKind::Parse, 0)),
            ContainerKind::Group => self.add_row(container.elements),
            ContainerKind::LeftRight { open, close } => {
                let style_override = container.structural_style_override;
                let body = self.add_row(container.elements)?;
                self.add_node(MathNode::Fenced { open, close, body, style_override })
            },
            ContainerKind::Matrix(mut matrix) => {
                let row_is_open = !container.elements.is_empty()
                    || matrix.cells.len() > matrix.current_row_start
                    || matrix.row_lengths.is_empty();
                if row_is_open {
                    if self.matrix_cell_count >= self.limits.max_matrix_cells {
                        return Err(MathError::new(MathErrorKind::MatrixCellLimit, 0));
                    }
                    let cell = self.add_row(mem::take(&mut container.elements))?;
                    matrix.cells.push(cell);
                    self.matrix_cell_count += 1;
                    let len = matrix.cells.len().saturating_sub(matrix.current_row_start);
                    if len > self.limits.max_children || len > u16::MAX as usize {
                        return Err(MathError::new(MathErrorKind::ChildLimit, 0));
                    }
                    matrix.row_lengths.push(len as u16);
                }
                self.add_matrix(matrix, container.structural_style_override)
            },
        }
    }

    fn add_matrix(
        &mut self,
        matrix: MatrixBuild,
        style_override: Option<StyleOverride>,
    ) -> Result<NodeId, MathError> {
        let row_start = self.arena.matrix_rows.len();
        let mut cell_start = 0usize;
        for length in &matrix.row_lengths {
            let end = cell_start + *length as usize;
            let cells = self.add_children(&matrix.cells[cell_start..end])?;
            self.arena.matrix_rows.push(MatrixRow { cells });
            cell_start = end;
        }
        if row_start > u32::MAX as usize || matrix.row_lengths.len() > u16::MAX as usize {
            return Err(MathError::new(MathErrorKind::NodeLimit, 0));
        }
        let rows = RowRange { start: row_start as u32, len: matrix.row_lengths.len() as u16 };

        if matrix.alignments.len() > self.limits.max_children
            || matrix.alignments.len() > u16::MAX as usize
            || self.arena.alignments.len() > u32::MAX as usize
        {
            return Err(MathError::new(MathErrorKind::ChildLimit, 0));
        }
        let alignments = AlignRange {
            start: self.arena.alignments.len() as u32,
            len: matrix.alignments.len() as u16,
        };
        self.arena.alignments.extend(matrix.alignments);
        let node = self.add_node(MathNode::Matrix { rows, alignments, style_override })?;
        if let Some((open, close)) = matrix.fence {
            self.add_node(MathNode::Fenced { open, close, body: node, style_override })
        } else {
            Ok(node)
        }
    }

    fn content_node(
        &mut self,
        content: Content<'_>,
        state: ParseState,
    ) -> Result<NodeId, MathError> {
        match content {
            Content::Text(text) => self.characters_node(
                text.chars(),
                AtomClass::Ord,
                ParseState { variant: FontVariant::UpRight, ..state },
            ),
            Content::Number(number) => self.characters_node(number.chars(), AtomClass::Ord, state),
            Content::Function(name) => {
                let body = self.characters_node(
                    name.chars(),
                    AtomClass::Ord,
                    ParseState { variant: FontVariant::UpRight, ..state },
                )?;
                self.add_node(MathNode::OperatorName {
                    body,
                    limits: is_limit_operator(name),
                    style_override: state.style_override,
                })
            },
            Content::Ordinary { content, stretchy } => {
                self.add_glyph(content, AtomClass::Ord, state, stretchy, None)
            },
            Content::LargeOp { content, small } => self.add_node(MathNode::Operator {
                character: content,
                variant: state.variant,
                style_override: state.style_override,
                small,
            }),
            Content::BinaryOp { content, .. } => {
                self.add_glyph(content, AtomClass::Bin, state, false, None)
            },
            Content::Relation { content, .. } => {
                let mut buffer = [0u8; 8];
                let encoded = content.encode_utf8_to_buf(&mut buffer);
                let text = std::str::from_utf8(encoded)
                    .map_err(|_| MathError::new(MathErrorKind::Parse, 0))?;
                self.characters_node(text.chars(), AtomClass::Rel, state)
            },
            Content::Delimiter { content, size, ty } => {
                let class = match ty {
                    DelimiterType::Open => AtomClass::Open,
                    DelimiterType::Close => AtomClass::Close,
                    DelimiterType::Fence => AtomClass::Inner,
                };
                self.add_glyph(content, class, state, false, size.map(delimiter_scale))
            },
            Content::Punctuation(character) => {
                self.add_glyph(character, AtomClass::Punct, state, false, None)
            },
        }
    }

    fn characters_node(
        &mut self,
        characters: impl Iterator<Item = char>,
        class: AtomClass,
        state: ParseState,
    ) -> Result<NodeId, MathError> {
        let mut nodes = Vec::new();
        for character in characters {
            if nodes.len() >= self.limits.max_children {
                return Err(MathError::new(MathErrorKind::ChildLimit, 0));
            }
            nodes.push(self.add_glyph(character, class, state, false, None)?);
        }
        if let [node] = nodes.as_slice() { Ok(*node) } else { self.add_row(nodes) }
    }

    fn add_glyph(
        &mut self,
        character: char,
        class: AtomClass,
        state: ParseState,
        stretchy: bool,
        delimiter_scale: Option<DelimiterScale>,
    ) -> Result<NodeId, MathError> {
        self.add_node(MathNode::Glyph {
            character,
            class,
            variant: state.variant,
            style_override: state.style_override,
            stretchy,
            delimiter_scale,
        })
    }

    fn add_row(&mut self, elements: Vec<NodeId>) -> Result<NodeId, MathError> {
        let children = self.add_children(&elements)?;
        self.add_node(MathNode::Row(children))
    }

    fn add_children(&mut self, elements: &[NodeId]) -> Result<ChildRange, MathError> {
        if elements.len() > self.limits.max_children || elements.len() > u16::MAX as usize {
            return Err(MathError::new(MathErrorKind::ChildLimit, 0));
        }
        if self.arena.children.len() > u32::MAX as usize {
            return Err(MathError::new(MathErrorKind::NodeLimit, 0));
        }
        let range =
            ChildRange { start: self.arena.children.len() as u32, len: elements.len() as u16 };
        self.arena.children.extend_from_slice(elements);
        Ok(range)
    }

    fn add_node(&mut self, node: MathNode) -> Result<NodeId, MathError> {
        if self.arena.nodes.len() >= self.limits.max_nodes
            || self.arena.nodes.len() >= u16::MAX as usize
        {
            return Err(MathError::new(MathErrorKind::NodeLimit, 0));
        }
        let id = NodeId(self.arena.nodes.len() as u16);
        self.arena.nodes.push(node);
        Ok(id)
    }

    fn finish(mut self) -> Result<(MathArena, NodeId), MathError> {
        if self.contexts.len() != 1 {
            return Err(MathError::new(MathErrorKind::Parse, 0));
        }
        let Some(Context::Container(root)) = self.contexts.pop() else {
            return Err(MathError::new(MathErrorKind::Parse, 0));
        };
        let root = self.add_row(root.elements)?;
        Ok((self.arena, root))
    }
}

fn font_variant(font: Font) -> FontVariant {
    match font {
        Font::BoldScript => FontVariant::BoldScript,
        Font::BoldItalic => FontVariant::BoldItalic,
        Font::Bold => FontVariant::Bold,
        Font::Fraktur => FontVariant::Fraktur,
        Font::Script => FontVariant::Script,
        Font::Monospace => FontVariant::Monospace,
        Font::SansSerif => FontVariant::SansSerif,
        Font::DoubleStruck => FontVariant::DoubleStruck,
        Font::Italic => FontVariant::Italic,
        Font::BoldFraktur => FontVariant::BoldFraktur,
        Font::SansSerifBoldItalic => FontVariant::SansSerifBoldItalic,
        Font::SansSerifItalic => FontVariant::SansSerifItalic,
        Font::BoldSansSerif => FontVariant::BoldSansSerif,
        Font::UpRight => FontVariant::UpRight,
    }
}

fn math_style(style: Style) -> MathStyle {
    match style {
        Style::Display => MathStyle::Display,
        Style::Text => MathStyle::Text,
        Style::Script => MathStyle::Script,
        Style::ScriptScript => MathStyle::ScriptScript,
    }
}

fn script_placement(position: ScriptPosition) -> ScriptPlacement {
    match position {
        ScriptPosition::Right => ScriptPlacement::Right,
        ScriptPosition::AboveBelow => ScriptPlacement::AboveBelow,
        ScriptPosition::Movable => ScriptPlacement::Movable,
    }
}

fn delimiter_scale(size: DelimiterSize) -> DelimiterScale {
    match size {
        DelimiterSize::Big => DelimiterScale::Big,
        DelimiterSize::BIG => DelimiterScale::BigUpper,
        DelimiterSize::Bigg => DelimiterScale::Bigg,
        DelimiterSize::BIGG => DelimiterScale::BiggUpper,
    }
}

fn column_align(alignment: ColumnAlignment) -> ColumnAlign {
    match alignment {
        ColumnAlignment::Left => ColumnAlign::Left,
        ColumnAlignment::Center => ColumnAlign::Center,
        ColumnAlignment::Right => ColumnAlign::Right,
    }
}

fn matrix_shape(grouping: &Grouping) -> (Vec<ColumnAlign>, Option<(Option<char>, Option<char>)>) {
    match grouping {
        Grouping::Array(columns) => (
            columns
                .iter()
                .filter_map(|column| match column {
                    ArrayColumn::Column(alignment) => Some(column_align(*alignment)),
                    ArrayColumn::Separator(_) => None,
                })
                .collect(),
            None,
        ),
        Grouping::Matrix { alignment } | Grouping::SubArray { alignment } => {
            (vec![column_align(*alignment)], None)
        },
        Grouping::Cases { left } => (
            vec![ColumnAlign::Left, ColumnAlign::Left],
            Some(if *left { (Some('{'), None) } else { (None, Some('}')) }),
        ),
        Grouping::Align { .. }
        | Grouping::Aligned
        | Grouping::Split
        | Grouping::Alignat { .. }
        | Grouping::Alignedat { .. } => (vec![ColumnAlign::Right, ColumnAlign::Left], None),
        Grouping::Equation { .. }
        | Grouping::Gather { .. }
        | Grouping::Gathered
        | Grouping::Multline => (vec![ColumnAlign::Center], None),
        Grouping::Normal | Grouping::LeftRight(_, _) => unreachable!(),
    }
}

fn is_limit_operator(name: &str) -> bool {
    matches!(
        name,
        "lim" | "liminf" | "limsup" | "max" | "min" | "sup" | "inf" | "det" | "gcd" | "Pr"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::ir::{AtomClass, FontVariant, MathNode, NodeId, ScriptPlacement};
    use crate::math::{DEFAULT_LIMITS, MathErrorKind};

    fn parse(source: &str) -> ParsedFormula {
        parse_formula(source, false, DEFAULT_LIMITS).unwrap()
    }

    fn root_children(formula: &ParsedFormula) -> &[NodeId] {
        let MathNode::Row(range) = formula.arena.node(formula.root) else { panic!() };
        formula.arena.children(*range)
    }

    #[test]
    fn parses_atoms_fraction_radical_and_scripts_into_arena() {
        let formula = parse(r"x_i^2+\frac{1}{\sqrt{y}}");
        assert!(formula.arena.nodes.len() <= DEFAULT_LIMITS.max_nodes);
        let children = root_children(&formula);
        assert_eq!(children.len(), 3);
        let MathNode::Scripts { placement, subscript, superscript, .. } =
            formula.arena.node(children[0])
        else {
            panic!("expected scripts")
        };
        assert_eq!(*placement, ScriptPlacement::Right);
        assert!(subscript.is_some() && superscript.is_some());
        assert!(matches!(formula.arena.node(children[2]), MathNode::Fraction { .. }));
    }

    #[test]
    fn relation_and_font_state_keep_semantic_atom_data() {
        let formula = parse(r"\mathbb{R} \le x");
        let children = root_children(&formula);
        let MathNode::Row(styled) = formula.arena.node(children[0]) else { panic!() };
        let MathNode::Glyph { character: 'R', variant, .. } =
            formula.arena.node(formula.arena.children(*styled)[0])
        else {
            panic!()
        };
        assert_eq!(*variant, FontVariant::DoubleStruck);
        assert!(children.iter().any(|id| matches!(
            formula.arena.node(*id),
            MathNode::Glyph { class: AtomClass::Rel, .. }
        )));
    }

    #[test]
    fn left_right_and_matrix_preserve_structure() {
        let formula = parse(r"\left(\begin{matrix}a&b\\c&d\end{matrix}\right)");
        let [fenced] = root_children(&formula) else { panic!() };
        let MathNode::Fenced { open: Some('('), close: Some(')'), body, .. } =
            formula.arena.node(*fenced)
        else {
            panic!("expected fenced matrix")
        };
        let MathNode::Row(body_children) = formula.arena.node(*body) else { panic!() };
        let [matrix] = formula.arena.children(*body_children) else { panic!() };
        let MathNode::Matrix { rows, .. } = formula.arena.node(*matrix) else { panic!() };
        let rows = formula.arena.rows(*rows);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| formula.arena.children(row.cells).len() == 2));
    }

    #[test]
    fn cases_adds_only_the_implicit_left_brace() {
        let formula = parse(r"\begin{cases}x&x>0\\-x&x\le0\end{cases}");
        let [fenced] = root_children(&formula) else { panic!() };
        let MathNode::Fenced { open: Some('{'), close: None, body, .. } =
            formula.arena.node(*fenced)
        else {
            panic!("expected implicit cases brace")
        };
        let MathNode::Matrix { rows, alignments, .. } = formula.arena.node(*body) else { panic!() };
        assert_eq!(formula.arena.rows(*rows).len(), 2);
        assert_eq!(
            formula.arena.row_alignments(*alignments),
            &[crate::math::ir::ColumnAlign::Left, crate::math::ir::ColumnAlign::Left]
        );
        assert!(formula.event_count > 0);
        assert_eq!(formula.style, crate::math::ir::MathStyle::Text);
    }

    #[test]
    fn accents_and_display_movable_limits_stay_distinct() {
        let accent = parse(r"\hat{x}");
        assert!(matches!(accent.arena.node(root_children(&accent)[0]), MathNode::Accent { .. }));

        let display = parse_formula(r"\sum_{i=0}^{n}", true, DEFAULT_LIMITS).unwrap();
        let MathNode::Scripts { placement, .. } = display.arena.node(root_children(&display)[0])
        else {
            panic!()
        };
        assert_eq!(*placement, ScriptPlacement::Movable);
    }

    #[test]
    fn parser_enforces_event_node_and_matrix_budgets() {
        let mut event_limits = DEFAULT_LIMITS;
        event_limits.max_events = 2;
        assert_eq!(
            parse_formula("abc", false, event_limits).unwrap_err().kind,
            MathErrorKind::EventLimit
        );

        let mut node_limits = DEFAULT_LIMITS;
        node_limits.max_nodes = 2;
        assert_eq!(
            parse_formula("abc", false, node_limits).unwrap_err().kind,
            MathErrorKind::NodeLimit
        );

        let mut matrix_limits = DEFAULT_LIMITS;
        matrix_limits.max_matrix_cells = 1;
        assert_eq!(
            parse_formula(r"\begin{matrix}a&b\end{matrix}", false, matrix_limits).unwrap_err().kind,
            MathErrorKind::MatrixCellLimit
        );
    }

    #[test]
    fn validation_runs_before_pulldown_latex() {
        assert_eq!(
            parse_formula(r"\def\x{1}\x", false, DEFAULT_LIMITS).unwrap_err().kind,
            MathErrorKind::ForbiddenCommand
        );
    }

    #[test]
    fn display_math_accepts_unscoped_line_breaks_without_rewriting_nested_matrices() {
        let multiline = parse_formula(r"x=1\\y=2", true, DEFAULT_LIMITS).unwrap();
        assert!(multiline.arena.nodes.iter().any(|node| matches!(node, MathNode::Matrix { .. })));

        assert!(!has_unscoped_line_break(r"\begin{matrix}a\\b\end{matrix}"));
        assert!(has_unscoped_line_break(r"\begin{matrix}a\\b\end{matrix}\\c"));
    }
}
