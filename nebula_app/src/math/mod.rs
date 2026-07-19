//! 原生 TeX 数学解析、排版与后端无关绘制指令。
//!
//! 该模块不依赖窗口、OpenGL 或主题类型，保证同一份布局可被不同渲染后端复用。

pub(crate) mod cache;
pub(crate) mod font;
pub(crate) mod ir;
pub(crate) mod layout;
pub(crate) mod parser;
pub(crate) mod rasterizer;
pub(crate) mod spacing;
pub(crate) mod validate;

pub(crate) use parser::parse_formula;
pub(crate) use validate::validate;

pub(crate) const DEFAULT_LIMITS: MathLimits = MathLimits {
    max_source_bytes: 16 * 1024,
    max_depth: 64,
    max_events: 8192,
    max_nodes: 4096,
    max_matrix_cells: 1024,
    max_children: 1024,
    max_ops: 8192,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MathLimits {
    pub(crate) max_source_bytes: usize,
    pub(crate) max_depth: usize,
    pub(crate) max_events: usize,
    pub(crate) max_nodes: usize,
    pub(crate) max_matrix_cells: usize,
    pub(crate) max_children: usize,
    pub(crate) max_ops: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MathErrorKind {
    SourceTooLong,
    NestingTooDeep,
    ForbiddenCommand,
    UnbalancedGroup,
    UnbalancedEnvironment,
    EventLimit,
    NodeLimit,
    MatrixCellLimit,
    ChildLimit,
    OpLimit,
    Parse,
    MissingGlyph,
    GlyphTooLarge,
    AtlasFull,
    Font,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MathError {
    pub(crate) kind: MathErrorKind,
    pub(crate) source_offset: u32,
}

impl MathError {
    pub(crate) fn new(kind: MathErrorKind, source_offset: usize) -> Self {
        Self { kind, source_offset: source_offset.min(u32::MAX as usize) as u32 }
    }
}
