//! Nebula 的 UI 层：所有自绘 chrome 的唯一配方来源。
//!
//! # 这一层为什么存在
//!
//! 2026-07-29 的侦察结果：`(x-1, y-1, w+2, h+2)` 的手写描边配方在 `display/`
//! 下出现 **95 处**；八个浮层用了 **六种**不同圆角（14/12/12/10/9/8）；
//! `tokens` 里明明定义好了 `space::M = 16`，代码里却有 26 个手写的 `s(16.0)`。
//!
//! 后果是改一条视觉规则要动十个文件，且必然漏掉几处——「统一配色」在这种结构
//! 下不可能做到。本层把配方收敛成函数，让视觉规则只有一个落点。
//!
//! # 层契约（新增代码必须遵守）
//!
//! 1. **纯几何 + quad 输出。** 不持有 [`Display`](crate::display::Display) 状态，
//!    不做 hover 决策，不读配置。输入是矩形和 [`Skin`](theme::Skin)，输出是 quad。
//!
//! 2. **布局、绘制、命中检测调同一个 rect helper。** 一个控件的可点击区域永远
//!    不会与它的绘制区域漂移——这类 bug 曾让手写的逐页控件极难调试。
//!
//! 3. **文字不在这里画。** 渲染器的文字是独立 pass（避免不透明 cell 背景），
//!    本层只暴露几何（`*_rect`、`layout_*`），文字 pass 复用同一份坐标。
//!
//! 4. **不写魔数。** 尺寸走 [`tokens::space`]，圆角走 [`tokens::radius`]，
//!    阴影走 [`tokens::elevation`]。`tokens` 没有的值说明该往 `tokens` 加。
//!
//! # 模块分工
//!
//! - [`tokens`] —— 间距、圆角、字阶、动效、层级的常量目录
//! - [`theme`] —— [`Skin`](theme::Skin)：主题派生的墨色与表面色
//! - [`surface`] —— 面板/卡片/描边/遮罩的配方（本层的核心）
//! - [`text_field`] —— 输入框的光标、选区、命中测试（唯一有可变状态的组件）
//! - [`widgets`] —— slider / toggle / combobox / spinner
//! - [`toast`] —— 自动消失的轻提示浮层
//! - [`keycap`] —— 键帽 chip
//! - [`icons`] —— 图标墨迹
//! - [`guardrails`] —— 把上面这些契约变成会失败的测试
//!
//! # 契约怎么被强制
//!
//! 第 4 条（不写魔数）曾经只写在这段注释里，然后 `widgets.rs` 自己就违反了
//! 它——只靠注释表达的约束等于没有约束。现在 [`guardrails`] 用数量上限测试盯着：
//! 新代码引入违规会让 `cargo test` 变红，且错误信息直接给出正确写法。
//!
//! # 未来
//!
//! 本目录的内容就是将来 `nebula_ui` crate 的内容。抽 crate 的前置条件是打破
//! `renderer` ↔ `display` 的循环依赖（`renderer/ui.rs` 用 `display::SizeInfo`
//! 和 `display::color::Rgb`，而 `theme.rs` 反过来用 `renderer::ui::Rgba`），
//! 需要先把 `Rgb`/`Rgba`/`UiQuad`/`SizeInfo` 这批纯数据类型下沉到共享底座。
//! 那一步的收益是编译器强制单向依赖 + 独立编译单元（改 UI 不再重编译整个 app）。

pub mod caret;
mod color_math;
pub mod guardrails;
pub(crate) mod geometry;
pub mod icons;
pub mod keycap;
mod keycap_model;
pub mod os_icons;
pub mod overlay_list;
mod scrollbar;
pub mod surface;
pub mod text_field;
pub mod theme;
pub mod toast;
mod toast_kind;
pub mod tokens;
pub mod widgets;
