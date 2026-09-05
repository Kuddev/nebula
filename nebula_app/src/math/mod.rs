//! 原生 TeX 数学解析、排版与后端无关绘制指令。
//!
//! 该模块不依赖窗口、OpenGL 或主题类型，保证同一份布局可被不同渲染后端复用。

pub(crate) mod cache;
mod compile;
pub(crate) mod font;
pub(crate) mod ir;
pub(crate) mod layout;
pub(crate) mod parser;
pub(crate) mod rasterizer;
pub(crate) mod spacing;
pub(crate) mod validate;

pub(crate) use compile::{compile_formula, compile_formula_source};
pub(crate) use parser::parse_formula;
pub(crate) use validate::validate;

/// 低于该字号的公式已不可读：调用方应放弃覆盖渲染，回退到原始源码文本。
/// 终端覆盖层与 markdown 阅读器共用同一条底线，保证两条管线判定一致。
pub(crate) const MIN_READABLE_MATH_PX: f32 = 6.0;

/// Latin Modern Math 在等宽字体旁边天生显小：它的 x-height 是 0.431 em，
/// 而主流编程字体在 0.53–0.56 em。同样的名义字号下，公式里的 `x` 比正文的
/// `x` 矮一圈。KaTeX 对同族字体给出的补偿是 1.21 em，这里沿用同一数值，
/// 在 [`compile_formula`] 单点生效，两条渲染管线（终端覆盖层 / markdown
/// 阅读器）自动一致。
pub(crate) const OPTICAL_SCALE: f32 = 1.21;

/// 窗口 DPI scale → `compile_formula` 的 `pixels_per_point`。TeX 的 pt 是
/// 1/72.27 英寸，逻辑分辨率按 96 dpi 计；该参数只决定 `\kern`/`\\[6pt]`
/// 这类显式长度的换算。两壳所有编译调用共用这一个公式，显式行距/间距
/// 才不会因壳而异（GPUI 壳曾直接传 scale factor，显式长度偏小 1/3）。
pub(crate) fn pixels_per_point(scale_factor: f32) -> f32 {
    scale_factor * 96.0 / 72.27
}

/// 上下标 / 分子分母的最小相对字号。Latin Modern 的 MATH 表给 70%，是为
/// 纸面阅读字号调的；终端正文本来就小，0.7 em 的分子在 20px 字号下只有
/// 14px，用户看到的就是"公式比正文小一圈"。这里抬到 0.8，代价是公式变高
/// 约一成——仍在一行加行距的预算内（见 `terminal_math` 的溢出容差），所以
/// 同类公式的字号不会因此分裂。
///
/// 这是刻意偏离 LaTeX/KaTeX 的一处：那两者排的是版面充裕的文档，我们排的
/// 是终端里 AI 输出的一行文字。上限卡在 0.8：再往上（实测 0.85）根号里的
/// 上标会把 `\sqrt` 顶过字形变体的阈值，高度从 30px 跳到 45px，那条公式
/// 就得缩——同类公式一样大比多两个百分点重要。
pub(crate) const MIN_SCRIPT_SCALE: f32 = 0.8;
/// 二级下标（`x^{a^b}` 的 b、分式套分式的内层）的下限。仍比一级小一档，
/// 层级关系保住，但不再掉到 0.5 em 那种糊成一团的尺寸。
pub(crate) const MIN_SCRIPT_SCRIPT_SCALE: f32 = 0.65;

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
