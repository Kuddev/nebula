//! 终端垂直切片：nebula_terminal（PTY/网格）× GPUI（渲染/输入/IME）。
//!
//! 边界：本模块之外不出现 nebula_terminal 类型；nebula_terminal 之内不出现
//! GPUI 类型。会话生命周期归 `session`，绘制归 `element`，交互归 `view`。

pub mod colors;
pub mod element;
pub mod keymap;
pub mod mouse_protocol;
pub mod session;
pub mod view;
