//! GPUI 正式产品复用的 UI/终端领域模型。
//!
//! 旧壳过去把模型和 OpenGL 绘制都放在 `display` 下；本门面只接入 GPUI
//! 当前需要的无窗口、无 GL、无 crossfont 部分。后续模型继续从旧目录迁出时，
//! 对外路径保持稳定，正式产品也不会重新依赖旧渲染器。

#[path = "../display/color.rs"]
pub mod color;
#[path = "../display/i18n.rs"]
mod i18n;
#[path = "../display/size_info.rs"]
mod size_info;

pub use i18n::{LanguagePreference, UiLanguage};
pub use size_info::SizeInfo;
