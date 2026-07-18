//! 有界、连续存储的数学中间表示。

use pulldown_latex::event::Dimension;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct NodeId(pub(crate) u16);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ChildRange {
    pub(crate) start: u32,
    pub(crate) len: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RowRange {
    pub(crate) start: u32,
    pub(crate) len: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AlignRange {
    pub(crate) start: u32,
    pub(crate) len: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AtomClass {
    Ord,
    Op,
    Bin,
    Rel,
    Open,
    Close,
    Punct,
    Inner,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum FontVariant {
    #[default]
    Normal,
    BoldScript,
    BoldItalic,
    Bold,
    Fraktur,
    Script,
    Monospace,
    SansSerif,
    DoubleStruck,
    Italic,
    BoldFraktur,
    SansSerifBoldItalic,
    SansSerifItalic,
    BoldSansSerif,
    UpRight,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum MathStyle {
    Display,
    #[default]
    Text,
    Script,
    ScriptScript,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScriptPlacement {
    Right,
    AboveBelow,
    Movable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DelimiterScale {
    Big,
    BigUpper,
    Bigg,
    BiggUpper,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ColumnAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MatrixRow {
    pub(crate) cells: ChildRange,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MathNode {
    Glyph {
        character: char,
        class: AtomClass,
        variant: FontVariant,
        style: MathStyle,
        stretchy: bool,
        delimiter_scale: Option<DelimiterScale>,
    },
    Operator {
        character: char,
        variant: FontVariant,
        style: MathStyle,
        small: bool,
    },
    OperatorName {
        body: NodeId,
        limits: bool,
    },
    Row(ChildRange),
    Fraction {
        numerator: NodeId,
        denominator: NodeId,
        bar: bool,
    },
    Scripts {
        base: NodeId,
        subscript: Option<NodeId>,
        superscript: Option<NodeId>,
        placement: ScriptPlacement,
    },
    Radical {
        degree: Option<NodeId>,
        body: NodeId,
    },
    Fenced {
        open: Option<char>,
        close: Option<char>,
        body: NodeId,
    },
    Accent {
        accent: char,
        body: NodeId,
    },
    Matrix {
        rows: RowRange,
        alignments: AlignRange,
    },
    Space {
        width: Option<Dimension>,
        height: Option<Dimension>,
    },
    Negation {
        body: NodeId,
    },
}

#[derive(Debug, Default)]
pub(crate) struct MathArena {
    pub(crate) nodes: Vec<MathNode>,
    pub(crate) children: Vec<NodeId>,
    pub(crate) matrix_rows: Vec<MatrixRow>,
    pub(crate) alignments: Vec<ColumnAlign>,
}

impl MathArena {
    pub(crate) fn node(&self, id: NodeId) -> &MathNode {
        &self.nodes[id.0 as usize]
    }

    pub(crate) fn children(&self, range: ChildRange) -> &[NodeId] {
        let start = range.start as usize;
        &self.children[start..start + range.len as usize]
    }

    pub(crate) fn rows(&self, range: RowRange) -> &[MatrixRow] {
        let start = range.start as usize;
        &self.matrix_rows[start..start + range.len as usize]
    }

    pub(crate) fn row_alignments(&self, range: AlignRange) -> &[ColumnAlign] {
        let start = range.start as usize;
        &self.alignments[start..start + range.len as usize]
    }
}

#[derive(Debug)]
pub(crate) struct ParsedFormula {
    pub(crate) arena: MathArena,
    pub(crate) root: NodeId,
    pub(crate) style: MathStyle,
    pub(crate) event_count: usize,
}
