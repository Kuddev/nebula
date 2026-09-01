//! 默认 GPUI 产品需要的无状态欢迎页算法。
//!
//! 完整 `WindowContext` 属于旧 winit 壳；GPUI 只复用欢迎页命令生成和内嵌字符画，
//! 保持两条路径的欢迎内容一致，同时不把 glutin 窗口上下文带进产品构建。

#[path = "window_context/nebula_fetch_art.rs"]
mod nebula_fetch_art;
#[path = "window_context/welcome.rs"]
pub(crate) mod welcome;
